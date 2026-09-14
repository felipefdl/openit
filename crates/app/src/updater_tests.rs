use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use super::*;

const PACKAGE: &[u8] = include_bytes!("updater-fixtures/update.app.tar.gz");
const SIGNATURE: &str = include_str!("updater-fixtures/update.app.tar.gz.sig");
const PUBLIC_KEY: &str = include_str!("updater-fixtures/test.pub");

struct Server {
  endpoint: String,
  stop: Arc<AtomicBool>,
  thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
  fn new(tampered: bool) -> Self {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let endpoint = format!("http://{address}/latest.json");
    let target = cargo_packager_updater::target().unwrap();
    let manifest = serde_json::json!({
      "version": "0.5.0",
      "platforms": {
        target: {
          "url": format!("http://{address}/update.app.tar.gz"),
          "signature": SIGNATURE.trim(),
          "format": "app"
        }
      }
    })
    .to_string();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let thread = std::thread::spawn(move || {
      while !stopped.load(Ordering::Relaxed) {
        let (mut stream, _) = match listener.accept() {
          Ok(connection) => connection,
          Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
            std::thread::sleep(Duration::from_millis(5));
            continue;
          },
          Err(err) => panic!("fixture server: {err}"),
        };
        stream.set_nonblocking(false).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
          let count = stream.read(&mut buffer).unwrap();
          assert_ne!(count, 0);
          request.extend_from_slice(&buffer[..count]);
        }
        let body = if request.starts_with(b"GET /latest.json ") {
          manifest.as_bytes().to_vec()
        } else {
          let mut bytes = PACKAGE.to_vec();
          if tampered {
            bytes[0] ^= 1;
          }
          bytes
        };
        write!(
          stream,
          "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
          body.len()
        )
        .unwrap();
        stream.write_all(&body).unwrap();
      }
    });
    Self { endpoint, stop, thread: Some(thread) }
  }

  fn check(&self, current: &str) -> Option<Update> {
    UpdaterBuilder::new(
      current.parse().unwrap(),
      Config {
        endpoints: vec![self.endpoint.parse().unwrap()],
        pubkey: PUBLIC_KEY.trim().into(),
        windows: None,
      },
    )
    .timeout(Duration::from_secs(30))
    .build()
    .unwrap()
    .check()
    .unwrap()
  }
}

impl Drop for Server {
  fn drop(&mut self) {
    self.stop.store(true, Ordering::Relaxed);
    let result = self.thread.take().unwrap().join();
    if !std::thread::panicking() {
      result.unwrap();
    }
  }
}

#[test]
fn checks_version_and_verifies_download_signature() {
  let server = Server::new(false);
  assert!(server.check("0.5.0").is_none());
  assert!(server.check("0.6.0").is_none());
  let update = server.check("0.4.0").expect("newer release");
  assert_eq!(update.version, "0.5.0");
  assert_eq!(update.download().unwrap(), PACKAGE);
}

#[cfg(target_os = "macos")]
#[test]
fn installs_signed_bundle_and_preserves_executable_permission() {
  let server = Server::new(false);
  let root = tempfile::tempdir().unwrap();
  let app = root.path().join("OpenIt.app");
  std::fs::create_dir_all(&app).unwrap();
  std::fs::write(app.join("old-version"), "old").unwrap();
  let mut update = server.check("0.4.0").unwrap();
  update.extract_path = app.clone();
  let progress = AtomicU64::new(0);
  install_package(&mut update, &|percent| {
    progress.store(percent.into(), Ordering::Relaxed);
  })
  .unwrap();
  assert!(!app.join("old-version").exists());
  let output = std::process::Command::new(app.join("Contents/MacOS/deathpush"))
    .output()
    .unwrap();
  assert!(output.status.success());
  assert_eq!(output.stdout, b"updated\n");
  assert_eq!(progress.load(Ordering::Relaxed), 100);
}

#[cfg(target_os = "macos")]
#[test]
fn rejects_tampered_download_without_replacing_installed_app() {
  let server = Server::new(true);
  let root = tempfile::tempdir().unwrap();
  let app = root.path().join("OpenIt.app");
  std::fs::create_dir_all(&app).unwrap();
  std::fs::write(app.join("old-version"), "old").unwrap();
  let mut update = server.check("0.4.0").unwrap();
  update.extract_path = app.clone();
  assert!(install_package(&mut update, &|_| {}).is_err());
  assert_eq!(std::fs::read_to_string(app.join("old-version")).unwrap(), "old");
}
