//! Handing a file OpenIt has no reader for to the system's default application.

use std::path::Path;
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;
use std::time::{Duration, Instant};

use gpui_kit::App;
use openit_core::Error;
use openit_core::handoff::{RepeatGuard, hands_off};

/// The platform spawns the system opener on its own background task, so the
/// exit waits long enough for that task to start the process.
const EXIT_DELAY: Duration = Duration::from_millis(750);

/// Opens a path with whatever the operating system runs for its type.
pub trait SystemOpener: Send + Sync + 'static {
  /// Hands `path` to the platform.
  fn open(&self, path: &Path, cx: &App);
}

/// The platform's default application, reached through GPUI.
pub struct PlatformOpener;

impl SystemOpener for PlatformOpener {
  fn open(&self, path: &Path, cx: &App) {
    cx.open_with_system(path);
  }
}

/// Application state for the handoff: where files go, which ones already went,
/// and how many open requests are still undecided.
pub struct Handoff {
  opener: Arc<dyn SystemOpener>,
  guard: RepeatGuard,
  in_flight: usize,
  handed_off: bool,
}

impl Default for Handoff {
  fn default() -> Self {
    Self {
      opener: Arc::new(PlatformOpener),
      guard: RepeatGuard::default(),
      in_flight: 0,
      handed_off: false,
    }
  }
}

impl gpui_kit::Global for Handoff {}

/// Count one open request that has not resolved into a window or a handoff yet.
pub fn started(cx: &mut App) {
  let handoff = cx.default_global::<Handoff>();
  handoff.in_flight = handoff.in_flight.saturating_add(1);
}

/// One open request resolved without a handoff.
pub fn settled(cx: &mut App) {
  finish(cx);
}

/// One open request failed. A refusal of the file itself goes to the system's
/// default application; anything else stays OpenIt's problem.
pub fn failed(path: &Path, error: &Error, cx: &mut App) {
  if hands_off(error) {
    hand_off(path, cx);
  } else {
    tracing::error!(%error, "open failed");
  }
  finish(cx);
}

fn hand_off(path: &Path, cx: &mut App) {
  let opener = {
    let handoff = cx.default_global::<Handoff>();
    if handoff.guard.admit(path, Instant::now()) {
      handoff.handed_off = true;
      Some(Arc::clone(&handoff.opener))
    } else {
      None
    }
  };
  let Some(opener) = opener else {
    tracing::warn!(path = %path.display(), "the system opener handed this file back; not opening it again");
    return;
  };
  tracing::info!(path = %path.display(), "no reader for this file; opening it with the system's default application");
  opener.open(path, cx);
}

fn finish(cx: &mut App) {
  let (in_flight, handed_off) = {
    let handoff = cx.default_global::<Handoff>();
    handoff.in_flight = handoff.in_flight.saturating_sub(1);
    (handoff.in_flight, handoff.handed_off)
  };
  if !exits(in_flight, handed_off, cx.windows().len()) {
    return;
  }
  cx.spawn(async move |cx| {
    cx.background_executor().timer(EXIT_DELAY).await;
    cx.update(|cx| {
      let (in_flight, handed_off) = {
        let handoff = cx.default_global::<Handoff>();
        (handoff.in_flight, handoff.handed_off)
      };
      if exits(in_flight, handed_off, cx.windows().len()) {
        cx.quit();
      }
    });
  })
  .detach();
}

/// Last-window close: Linux and Windows quit; macOS stays resident for Dock reopen.
pub fn should_quit_after_last_window(cx: &App) -> bool {
  let in_flight = cx.try_global::<Handoff>().map_or(0, |handoff| handoff.in_flight);
  quits_when_empty(in_flight, cx.windows().len(), cfg!(target_os = "macos"))
}

/// A launch whose only outcome was a handoff has nothing to show and exits.
const fn exits(in_flight: usize, handed_off: bool, windows: usize) -> bool {
  empty_counts(in_flight, windows) && handed_off
}

const fn empty_counts(in_flight: usize, windows: usize) -> bool {
  in_flight == 0 && windows == 0
}

const fn quits_when_empty(in_flight: usize, windows: usize, macos: bool) -> bool {
  empty_counts(in_flight, windows) && !macos
}

/// Replaces the opener with one that records the paths it receives.
#[cfg(test)]
pub fn record_openings(cx: &mut App) -> Arc<Mutex<Vec<std::path::PathBuf>>> {
  let recorded = Arc::new(Mutex::new(Vec::new()));
  cx.default_global::<Handoff>().opener = Arc::new(Recorder(Arc::clone(&recorded)));
  recorded
}

#[cfg(test)]
struct Recorder(Arc<Mutex<Vec<std::path::PathBuf>>>);

#[cfg(test)]
impl SystemOpener for Recorder {
  fn open(&self, path: &Path, _cx: &App) {
    self
      .0
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(path.to_owned());
  }
}

#[cfg(test)]
mod tests {
  use super::{empty_counts, exits, quits_when_empty};

  #[test]
  fn only_a_finished_handoff_with_nothing_on_screen_exits() {
    assert!(empty_counts(0, 0));
    assert!(!empty_counts(1, 0), "another open request is still undecided");
    assert!(!empty_counts(0, 1), "a window is open");
    assert!(exits(0, true, 0));
    assert!(!exits(1, true, 0), "another open request is still undecided");
    assert!(!exits(0, true, 1), "a window is open");
    assert!(!exits(0, false, 0), "a launch with nothing to open keeps running");
  }

  #[test]
  fn last_window_quits_off_macos_and_stays_on_macos() {
    assert!(!quits_when_empty(0, 0, true), "macOS stays resident with no windows");
    assert!(quits_when_empty(0, 0, false), "Linux and Windows quit with no windows");
    assert!(!quits_when_empty(1, 0, false), "an in-flight open keeps the process");
    assert!(!quits_when_empty(0, 1, false), "a remaining window keeps the process");
  }
}
