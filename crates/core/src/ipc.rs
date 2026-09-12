//! Per-user local socket that forwards open requests to a running instance.

use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use interprocess::ConnectWaitMode;
use interprocess::local_socket::{
  ConnectOptions, GenericFilePath, ListenerOptions, Name, Stream, ToFsName, prelude::*,
};
use serde::{Deserialize, Serialize};

use crate::error::Error;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_REQUEST_BYTES: u64 = 1024 * 1024;
const MAX_REPLY_BYTES: u64 = 16;

/// No running OpenIt instance accepted the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoInstance;

impl std::fmt::Display for NoInstance {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("no running OpenIt instance")
  }
}

impl std::error::Error for NoInstance {}

/// Per-user socket path, or a Windows named pipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocketName(PathBuf);

impl SocketName {
  /// `<runtime_dir>/openit.sock` when XDG defines it, otherwise `<data_local_dir>/openit/openit.sock`.
  /// On Windows, `\\.\pipe\openit-<username>`.
  pub fn user() -> Result<Self, Error> {
    default_socket_name()
  }

  /// Build a name from a filesystem path or a `\\.\pipe\...` pipe path.
  pub fn from_path(path: impl Into<PathBuf>) -> Self {
    Self(path.into())
  }

  /// The path or pipe this name refers to.
  pub fn as_path(&self) -> &Path {
    &self.0
  }

  fn local_name(&self) -> io::Result<Name<'_>> {
    self.0.as_path().to_fs_name::<GenericFilePath>()
  }
}

/// Bound listener. Each accepted connection is one open request.
pub struct Listener {
  inner: interprocess::local_socket::Listener,
  name: SocketName,
}

impl std::fmt::Debug for Listener {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Listener").field("name", &self.name).finish_non_exhaustive()
  }
}

impl Listener {
  /// Accept one connection and read its path list. Reply `ok` after [`Incoming::take`].
  pub fn accept(&self) -> Result<Incoming, Error> {
    let stream = self.inner.accept().map_err(|source| ipc_error(self.name.as_path(), source))?;
    read_request(self.name.as_path(), stream)
  }
}

/// One request. The client receives `ok` only after [`Self::take`].
#[must_use = "take the paths so the client receives ok"]
pub struct Incoming {
  paths: Vec<PathBuf>,
  stream: Option<Stream>,
}

impl Incoming {
  /// Paths in this request, in the order they were sent. Writes the `ok` reply.
  pub fn take(mut self) -> Vec<PathBuf> {
    if let Some(mut stream) = self.stream.take()
      && let Err(error) = writeln_ok(&mut stream)
    {
      tracing::debug!(%error, "could not reply to open request");
    }
    self.paths
  }
}

/// Connect to the default socket, write `paths`, and wait for `ok`.
pub fn send<I, P>(paths: I) -> Result<(), NoInstance>
where
  I: IntoIterator<Item = P>,
  P: AsRef<Path>,
{
  let name = SocketName::user().map_err(no_instance)?;
  send_to(&name, paths)
}

/// [`send`] to an explicit socket name.
pub fn send_to<I, P>(name: &SocketName, paths: I) -> Result<(), NoInstance>
where
  I: IntoIterator<Item = P>,
  P: AsRef<Path>,
{
  let request = WireRequest {
    paths: paths.into_iter().map(|path| path.as_ref().to_path_buf()).collect(),
  };
  let mut payload = serde_json::to_string(&request).map_err(no_instance)?;
  payload.push('\n');

  let stream = connect(name).map_err(no_instance)?;
  stream.set_send_timeout(Some(REPLY_TIMEOUT)).map_err(no_instance)?;
  stream.set_recv_timeout(Some(REPLY_TIMEOUT)).map_err(no_instance)?;

  let mut stream = stream;
  stream.write_all(payload.as_bytes()).map_err(no_instance)?;

  let mut reply = String::new();
  let n = BufReader::new(stream)
    .take(MAX_REPLY_BYTES)
    .read_line(&mut reply)
    .map_err(no_instance)?;
  if n == 0 || reply.trim() != "ok" {
    return Err(NoInstance);
  }
  Ok(())
}

/// Bind the default socket, replacing a stale file when a connect attempt fails.
pub fn listen() -> Result<Listener, Error> {
  listen_at(&SocketName::user()?)
}

/// [`listen`] at an explicit socket name.
pub fn listen_at(name: &SocketName) -> Result<Listener, Error> {
  ensure_parent(name.as_path())?;
  match bind(name) {
    Ok(inner) => Ok(Listener { inner, name: name.clone() }),
    Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
      if connect(name).is_ok() {
        return Err(ipc_error(name.as_path(), error));
      }
      remove_stale(name.as_path())?;
      let inner = bind(name).map_err(|source| ipc_error(name.as_path(), source))?;
      Ok(Listener { inner, name: name.clone() })
    },
    Err(source) => Err(ipc_error(name.as_path(), source)),
  }
}

#[derive(Serialize, Deserialize)]
struct WireRequest {
  paths: Vec<PathBuf>,
}

fn default_socket_name() -> Result<SocketName, Error> {
  #[cfg(windows)]
  {
    let user = std::env::var("USERNAME").map_err(|error| Error::Ipc {
      path: PathBuf::from(r"\\.\pipe\openit"),
      source: io::Error::new(io::ErrorKind::NotFound, error),
    })?;
    return Ok(SocketName(PathBuf::from(format!(r"\\.\pipe\openit-{user}"))));
  }
  #[cfg(not(windows))]
  {
    if let Some(runtime) = dirs::runtime_dir() {
      return Ok(SocketName(runtime.join("openit.sock")));
    }
    if let Some(data) = dirs::data_local_dir() {
      return Ok(SocketName(data.join("openit").join("openit.sock")));
    }
    Err(Error::Ipc {
      path: PathBuf::from("openit.sock"),
      source: io::Error::new(io::ErrorKind::NotFound, "no runtime or local data directory"),
    })
  }
}

fn bind(name: &SocketName) -> io::Result<interprocess::local_socket::Listener> {
  ListenerOptions::new().name(name.local_name()?).create_sync()
}

fn connect(name: &SocketName) -> io::Result<Stream> {
  ConnectOptions::new()
    .name(name.local_name()?)
    .wait_mode(ConnectWaitMode::Timeout(CONNECT_TIMEOUT))
    .connect_sync()
}

fn read_request(path: &Path, stream: Stream) -> Result<Incoming, Error> {
  let mut reader = BufReader::new(stream);
  let mut line = String::new();
  let n = reader
    .by_ref()
    .take(MAX_REQUEST_BYTES.saturating_add(1))
    .read_line(&mut line)
    .map_err(|source| ipc_error(path, source))?;
  if n == 0 {
    return Err(ipc_error(path, io::Error::new(io::ErrorKind::InvalidData, "empty request")));
  }
  if u64::try_from(line.len()).unwrap_or(u64::MAX) > MAX_REQUEST_BYTES || !line.ends_with('\n') {
    return Err(ipc_error(
      path,
      io::Error::new(io::ErrorKind::InvalidData, "request exceeds the size cap"),
    ));
  }
  let request: WireRequest = serde_json::from_str(line.trim())
    .map_err(|error| ipc_error(path, io::Error::new(io::ErrorKind::InvalidData, error)))?;
  Ok(Incoming {
    paths: request.paths,
    stream: Some(reader.into_inner()),
  })
}

fn writeln_ok(stream: &mut Stream) -> io::Result<()> {
  stream.write_all(b"ok\n")?;
  stream.flush()
}

fn ensure_parent(path: &Path) -> Result<(), Error> {
  if cfg!(windows) {
    return Ok(());
  }
  let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) else {
    return Ok(());
  };
  std::fs::create_dir_all(parent).map_err(|source| ipc_error(path, source))
}

fn remove_stale(path: &Path) -> Result<(), Error> {
  if cfg!(windows) {
    return Ok(());
  }
  match std::fs::remove_file(path) {
    Ok(()) => Ok(()),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
    Err(source) => Err(ipc_error(path, source)),
  }
}

fn ipc_error(path: &Path, source: io::Error) -> Error {
  tracing::debug!(path = %path.display(), %source, "ipc failed");
  Error::Ipc { path: path.to_path_buf(), source }
}

fn no_instance(error: impl std::fmt::Display) -> NoInstance {
  tracing::debug!(%error, "no running instance");
  NoInstance
}

#[cfg(test)]
mod tests {
  use std::path::PathBuf;
  use std::time::{Duration, Instant};

  use super::{NoInstance, SocketName, listen_at, send_to};

  struct TestSocket {
    name: SocketName,
    _dir: Option<tempfile::TempDir>,
  }

  impl TestSocket {
    fn new() -> Self {
      #[cfg(windows)]
      {
        let id = uuid::Uuid::new_v4();
        Self {
          name: SocketName::from_path(format!(r"\\.\pipe\openit-test-{id}")),
          _dir: None,
        }
      }
      #[cfg(not(windows))]
      {
        let dir = tempfile::tempdir().unwrap();
        let name = SocketName::from_path(dir.path().join("openit.sock"));
        Self { name, _dir: Some(dir) }
      }
    }
  }

  #[test]
  fn send_round_trips_two_paths() {
    let test = TestSocket::new();
    let name = test.name.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
      let listener = listen_at(&name).unwrap();
      ready_tx.send(()).unwrap();
      listener.accept().unwrap().take()
    });
    ready_rx.recv_timeout(Duration::from_secs(2)).expect("listener");
    let paths = [PathBuf::from("/abs/a.md"), PathBuf::from("/abs/b.md")];
    send_to(&test.name, &paths).expect("client");
    let received = handle.join().expect("listener thread");
    assert_eq!(received, paths);
  }

  #[test]
  fn send_without_a_listener_is_no_instance() {
    let test = TestSocket::new();
    let start = Instant::now();
    let result = send_to(&test.name, std::iter::empty::<&std::path::Path>());
    assert_eq!(result, Err(NoInstance));
    assert!(start.elapsed() <= Duration::from_secs(2));
  }

  #[cfg(unix)]
  #[test]
  fn listen_binds_over_a_stale_socket_file() {
    let test = TestSocket::new();
    std::fs::write(test.name.as_path(), []).unwrap();
    let listener = listen_at(&test.name).expect("bind over stale file");
    drop(listener);
  }
}
