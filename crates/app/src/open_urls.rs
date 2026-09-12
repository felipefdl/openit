//! OS open-URL events. Tests call [`on_open_urls`]; the test platform panics on
//! `Application::on_open_urls`.

use std::path::PathBuf;

use gpui_kit::{App, AppContext, Global};

use openit_core::associations::paths_from_open_urls;

use crate::window::apply_open_request;

#[derive(Default)]
struct OpenUrlGate {
  ready: bool,
  pending: Vec<PathBuf>,
}

impl Global for OpenUrlGate {}

/// Decode `file://` URLs into one open request. Other schemes are logged and ignored.
#[allow(
  clippy::needless_pass_by_ref_mut,
  reason = "GPUI schedules work from a mutable application callback"
)]
pub fn on_open_urls(urls: Vec<String>, cx: &mut App) {
  let paths = paths_from_open_urls(urls);
  if paths.is_empty() {
    return;
  }
  if cx.has_global::<OpenUrlGate>() {
    let (ready, was_empty) = {
      let gate = cx.default_global::<OpenUrlGate>();
      (gate.ready, gate.pending.is_empty())
    };
    if !ready {
      if was_empty {
        crate::handoff::started(cx);
      }
      cx.default_global::<OpenUrlGate>().pending.extend(paths);
      return;
    }
  }
  apply_open_request(paths, cx);
}

/// Pump platform open-URL events through [`on_open_urls`].
///
/// Paths that arrive before [`startup_ready`] wait so they fill the empty
/// startup window instead of racing it.
#[allow(
  clippy::needless_pass_by_ref_mut,
  reason = "GPUI schedules work from a mutable application callback"
)]
pub fn listen(rx: async_channel::Receiver<Vec<String>>, cx: &mut App) {
  cx.set_global(OpenUrlGate { ready: false, pending: Vec::new() });
  cx.spawn(async move |cx| {
    loop {
      let received = cx
        .background_spawn({
          let rx = rx.clone();
          async move { rx.recv().await }
        })
        .await;
      let Ok(urls) = received else {
        break;
      };
      cx.update(|cx| on_open_urls(urls, cx));
    }
  })
  .detach();
}

/// Flush URLs that arrived while startup windows were still opening.
#[allow(
  clippy::needless_pass_by_ref_mut,
  reason = "GPUI schedules work from a mutable application callback"
)]
pub fn startup_ready(cx: &mut App) {
  if !cx.has_global::<OpenUrlGate>() {
    return;
  }
  let pending = {
    let gate = cx.default_global::<OpenUrlGate>();
    gate.ready = true;
    std::mem::take(&mut gate.pending)
  };
  if !pending.is_empty() {
    apply_open_request(pending, cx);
    crate::handoff::settled(cx);
  }
}

#[cfg(test)]
#[allow(clippy::items_after_statements, reason = "test-local cleanup type")]
mod tests {
  use std::fs;
  use std::io::Write;
  use std::path::PathBuf;
  use std::sync::{Arc, Mutex, PoisonError};

  use gpui_kit::TestAppContext;
  use openit_core::settings::Settings;

  use crate::document_view::DocumentView;

  use super::on_open_urls;

  fn init_app(cx: &TestAppContext) {
    cx.update(|cx| {
      gpui_kit::init(cx);
      cx.set_global(crate::settings::AppSettings(Settings::default()));
      cx.set_global(crate::theme::ThemeDirs::default());
      crate::theme::init(cx);
    });
  }

  struct Writer(Arc<Mutex<Vec<u8>>>);

  impl Write for Writer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
      self.0.lock().unwrap_or_else(PoisonError::into_inner).extend_from_slice(buf);
      Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
      Ok(())
    }
  }

  impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Writer {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
      Self(Arc::clone(&self.0))
    }
  }

  #[gpui_kit::test]
  fn open_urls_seam_opens_the_decoded_file_and_logs_other_schemes(cx: &mut TestAppContext) {
    init_app(cx);
    let path = PathBuf::from("/tmp/a b.md");
    fs::write(&path, "notes").unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
      fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
      }
    }
    let _cleanup = Cleanup(path.clone());

    let logs = Arc::new(Mutex::new(Vec::<u8>::new()));
    let subscriber = tracing_subscriber::fmt()
      .with_writer(Writer(Arc::clone(&logs)))
      .with_max_level(tracing::Level::INFO)
      .finish();
    tracing::subscriber::with_default(subscriber, || {
      cx.update(|cx| {
        on_open_urls(vec!["file:///tmp/a%20b.md".into(), "https://example.com/x.md".into()], cx);
      });
    });
    cx.run_until_parked();

    assert_eq!(cx.windows().len(), 1, "only the file url becomes an open request");
    let handle = cx
      .windows()
      .into_iter()
      .next()
      .and_then(|window| window.downcast::<DocumentView>())
      .expect("the file url opens a document");
    let opened = handle.update(cx, |view, _, _| view.path().to_path_buf()).unwrap();
    assert_eq!(opened, fs::canonicalize(&path).unwrap());
    let text = String::from_utf8_lossy(&logs.lock().unwrap_or_else(PoisonError::into_inner)).into_owned();
    assert!(text.contains("https://example.com/x.md"), "{text}");
  }
}
