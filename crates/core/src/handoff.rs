//! Which reader refusals belong to another application, and the guard against
//! a shell that hands the same file straight back.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::error::Error;

/// How long a handed-off path is refused a second handoff.
pub const REPEAT_WINDOW: Duration = Duration::from_secs(10);

/// Whether the operating system's default application should get this file.
///
/// True when the readers refuse the file itself: no reader for the name, bytes
/// that are not text, or a file over a reader's cap. False when the file could
/// not be read at all, or when a reader accepted it and failed on the content;
/// another application cannot do better with the first, and OpenIt reports the
/// second in the window it already opened.
pub const fn hands_off(error: &Error) -> bool {
  matches!(
    error,
    Error::Unsupported { .. } | Error::NotUtf8 { .. } | Error::TooLarge { .. }
  )
}

/// Remembers handed-off paths so a shell that routes the type back to OpenIt
/// cannot start a cycle.
#[derive(Debug, Default)]
pub struct RepeatGuard {
  recent: HashMap<PathBuf, Instant>,
}

impl RepeatGuard {
  /// Records `path` and reports whether this handoff may proceed. A repeat
  /// within [`REPEAT_WINDOW`] is refused.
  pub fn admit(&mut self, path: &Path, now: Instant) -> bool {
    self.recent.retain(|_, at| now.duration_since(*at) < REPEAT_WINDOW);
    match self.recent.entry(path.to_path_buf()) {
      Entry::Occupied(_) => false,
      Entry::Vacant(slot) => {
        slot.insert(now);
        true
      },
    }
  }
}

#[cfg(test)]
mod tests {
  use std::io;

  use super::{Instant, Path, PathBuf, REPEAT_WINDOW, RepeatGuard, hands_off};
  use crate::error::Error;

  fn path() -> PathBuf {
    PathBuf::from("/tmp/report.zip")
  }

  #[test]
  fn refusals_about_the_file_itself_go_to_the_system() {
    assert!(hands_off(&Error::Unsupported { path: path() }));
    assert!(hands_off(&Error::NotUtf8 { path: path() }));
    assert!(hands_off(&Error::TooLarge { path: path(), size: 9, limit: 8 }));
  }

  #[test]
  fn an_unreadable_or_broken_file_stays_with_openit() {
    assert!(!hands_off(&Error::Read {
      path: path(),
      source: io::Error::from(io::ErrorKind::NotFound),
    }));
    assert!(!hands_off(&Error::Decode {
      path: path(),
      reason: "truncated".to_owned(),
    }));
  }

  #[test]
  fn the_same_path_is_refused_a_second_handoff_inside_the_window() {
    let mut guard = RepeatGuard::default();
    let now = Instant::now();

    assert!(guard.admit(&path(), now));
    assert!(!guard.admit(&path(), now + REPEAT_WINDOW / 2));
    assert!(guard.admit(Path::new("/tmp/other.zip"), now));
  }

  #[test]
  fn a_later_reopen_is_admitted_again() {
    let mut guard = RepeatGuard::default();
    let now = Instant::now();

    assert!(guard.admit(&path(), now));

    assert!(guard.admit(&path(), now + REPEAT_WINDOW));
  }
}
