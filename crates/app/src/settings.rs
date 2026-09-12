//! Application settings loading, serialized persistence, and change watching.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::{App, AppContext, BorrowAppContext, Global, Task};
use openit_core::settings::Settings;
use openit_core::watch::{FileWatch, Fingerprint};

const SETTINGS_SETTLE: Duration = Duration::from_millis(150);

/// How long after the last edit autosave writes.
pub const AUTOSAVE_DELAY: Duration = Duration::from_secs(1);
/// The loaded settings, shared by every window.
pub struct AppSettings(pub Settings);

impl Global for AppSettings {}

/// Application-wide settings persistence and self-write tracking.
pub struct SettingsStore {
  path: Option<PathBuf>,
  generation: u64,
  /// Bumped every time the pump loads settings from the file on disk.
  file_generation: u64,
  pending: Option<Settings>,
  last_write_fingerprint: Option<Fingerprint>,
  last_error: Option<String>,
  write_task: Option<Task<()>>,
}

impl Global for SettingsStore {}

impl Default for SettingsStore {
  fn default() -> Self {
    Self::new(Settings::default_path())
  }
}

impl SettingsStore {
  /// Create a settings store writing to `path`.
  pub const fn new(path: Option<PathBuf>) -> Self {
    Self {
      path,
      generation: 0,
      file_generation: 0,
      pending: None,
      last_write_fingerprint: None,
      last_error: None,
      write_task: None,
    }
  }

  /// Update the application settings and enqueue one coalescing write.
  pub fn update(cx: &mut App, update: impl FnOnce(&mut Settings)) {
    if !cx.has_global::<AppSettings>() {
      cx.set_global(AppSettings(Settings::default()));
    }
    if !cx.has_global::<Self>() {
      cx.set_global(Self::default());
    }
    let mut settings = cx.global::<AppSettings>().0.clone();
    update(&mut settings);
    cx.set_global(AppSettings(settings.clone()));

    let start_writer = cx.update_global::<Self, _>(|store, _| {
      store.generation = store.generation.wrapping_add(1);
      store.pending = Some(settings);
      store.write_task.is_none()
    });
    if start_writer {
      let task = spawn_settings_writer(cx);
      cx.update_global::<Self, _>(|store, _| store.write_task = Some(task));
    }
  }

  #[cfg(test)]
  pub(crate) fn set_path(cx: &mut App, path: PathBuf) {
    if !cx.has_global::<Self>() {
      cx.set_global(Self::new(Some(path)));
      return;
    }
    cx.update_global::<Self, _>(|store, _| {
      store.path = Some(path);
      store.last_write_fingerprint = None;
    });
  }

  pub(crate) const fn take_write(&mut self) -> Option<Task<()>> {
    self.write_task.take()
  }

  /// How many times settings have been loaded from the settings file.
  pub(crate) const fn file_generation(&self) -> u64 {
    self.file_generation
  }

  pub(crate) fn last_error(&self) -> Option<&str> {
    self.last_error.as_deref()
  }

  const fn write_pending(&self) -> bool {
    self.pending.is_some() || self.write_task.is_some()
  }

  fn skips_reload(&self, fingerprint: Option<Fingerprint>) -> bool {
    fingerprint.is_some() && self.last_write_fingerprint == fingerprint
  }
}

fn spawn_settings_writer(cx: &App) -> Task<()> {
  cx.spawn(async move |cx| {
    loop {
      let (generation, path, settings) =
        cx.read_global::<SettingsStore, _>(|store, _| (store.generation, store.path.clone(), store.pending.clone()));
      let Some(settings) = settings else {
        let keep_going = cx.update_global::<SettingsStore, _>(|store, _| {
          if store.generation == generation {
            store.write_task = None;
            false
          } else {
            true
          }
        });
        if !keep_going {
          break;
        }
        continue;
      };
      let Some(path) = path else {
        let keep_going = cx.update_global::<SettingsStore, _>(|store, _| {
          if store.generation == generation {
            store.pending = None;
            store.write_task = None;
            false
          } else {
            true
          }
        });
        if !keep_going {
          break;
        }
        continue;
      };
      let result = cx
        .background_spawn(async move {
          let saved = settings.save(&path).map_err(|error| error.to_string());
          let fingerprint = saved.as_ref().ok().and_then(|()| Fingerprint::of(&path).ok());
          (saved, fingerprint)
        })
        .await;
      let keep_going = cx.update_global::<SettingsStore, _>(|store, _| {
        let (saved, fingerprint) = result;
        match saved {
          Ok(()) => {
            store.last_write_fingerprint = fingerprint;
            store.last_error = None;
          },
          Err(error) => {
            store.last_error = Some(error.clone());
            tracing::error!(%error, "settings save failed");
          },
        }
        if store.generation == generation {
          store.pending = None;
          store.write_task = None;
          false
        } else {
          true
        }
      });
      if !keep_going {
        break;
      }
    }
  })
}

#[expect(
  dead_code,
  reason = "the global keeps the settings watcher and reload task alive"
)]
pub struct SettingsWatch {
  watcher: Option<FileWatch>,
  task: Task<()>,
}

impl Global for SettingsWatch {}

/// Load settings changes from `path` after a short debounce and update the global.
pub fn watch_settings(path: PathBuf, cx: &mut App) {
  let Some(config_dir) = path.parent() else {
    tracing::warn!("settings path has no parent; settings file not watched");
    return;
  };
  if let Err(error) = fs::create_dir_all(config_dir) {
    tracing::warn!(%error, "settings directory unavailable; settings file not watched");
    return;
  }
  let (tx, rx) = async_channel::bounded::<()>(1);
  match FileWatch::new(&path, move || {
    let _ = tx.try_send(());
  }) {
    Ok(watch) => {
      let task = spawn_settings_pump(path, rx, cx);
      cx.set_global(SettingsWatch { watcher: Some(watch), task });
    },
    Err(error) => {
      tracing::warn!(%error, "settings file not watched");
    },
  }
}

/// A foreign edit observed while a local write is pending is intentionally
/// ignored; the queued local snapshot wins and overwrites that edit.
fn spawn_settings_pump(path: PathBuf, rx: async_channel::Receiver<()>, cx: &App) -> Task<()> {
  cx.spawn(async move |cx| {
    while rx.recv().await.is_ok() {
      cx.background_executor().timer(SETTINGS_SETTLE).await;
      while rx.try_recv().is_ok() {}
      let loaded = cx
        .background_spawn({
          let path = path.clone();
          async move { Settings::load(&path) }
        })
        .await;
      cx.update(|cx| match loaded {
        Ok(settings) => {
          let fingerprint = Fingerprint::of(&path).ok();
          let store = cx.global::<SettingsStore>();
          if store.write_pending() || store.skips_reload(fingerprint) {
            return;
          }
          cx.update_global::<SettingsStore, _>(|store, _| {
            store.last_write_fingerprint = None;
            store.file_generation = store.file_generation.wrapping_add(1);
          });
          cx.set_global(AppSettings(settings));
        },
        Err(error) => tracing::error!(%error, "settings reload failed; keeping current"),
      });
    }
  })
}

#[cfg(test)]
pub(crate) fn watch_settings_for_test(path: PathBuf, cx: &gpui_kit::TestAppContext) -> async_channel::Sender<()> {
  let (tx, rx) = async_channel::bounded::<()>(1);
  let task = cx.update(|cx| spawn_settings_pump(path, rx, cx));
  cx.update(|cx| {
    cx.set_global(SettingsWatch { watcher: None, task });
  });
  tx
}

#[cfg(test)]
mod tests {
  use std::fs;
  use std::time::Duration;

  use gpui_kit::TestAppContext;
  use openit_core::settings::Settings;

  use super::{AppSettings, SettingsStore, watch_settings_for_test};

  #[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "GPUI test setup uses a mutable application context"
  )]
  #[gpui_kit::test]
  fn settings_reload_updates_the_global_after_a_file_change(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    cx.update(|cx| {
      cx.set_global(AppSettings(Settings::default()));
      cx.set_global(SettingsStore::new(None));
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    let sender = watch_settings_for_test(path.clone(), cx);

    fs::write(&path, "autosave = true\n").unwrap();
    sender.try_send(()).unwrap();
    cx.executor().advance_clock(Duration::from_millis(150));
    cx.run_until_parked();

    assert!(cx.read_global::<AppSettings, _>(|settings, _| settings.0.autosave));
  }

  #[gpui_kit::test]
  fn a_watcher_event_for_the_store_write_does_not_replace_global_settings(cx: &TestAppContext) {
    cx.update(gpui_kit::init);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    cx.update(|cx| {
      cx.set_global(AppSettings(Settings::default()));
      cx.set_global(SettingsStore::new(Some(path.clone())));
      SettingsStore::update(cx, |settings| settings.autosave = true);
    });
    cx.run_until_parked();
    assert!(Settings::load(&path).unwrap().autosave);

    cx.update(|cx| cx.set_global(AppSettings(Settings::default())));
    let sender = watch_settings_for_test(path.clone(), cx);
    sender.try_send(()).unwrap();
    cx.executor().advance_clock(Duration::from_millis(150));
    cx.run_until_parked();

    assert!(!cx.read_global::<AppSettings, _>(|settings, _| settings.0.autosave));

    fs::write(&path, "autosave = true\nallow_remote = true\n").unwrap();
    sender.try_send(()).unwrap();
    cx.executor().advance_clock(Duration::from_millis(150));
    cx.run_until_parked();

    assert!(cx.read_global::<AppSettings, _>(|settings, _| settings.0.allow_remote));
  }

  #[gpui_kit::test]
  fn a_pending_settings_snapshot_wins_over_a_stale_watcher_event(cx: &TestAppContext) {
    cx.update(gpui_kit::init);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    let family = openit_core::resource::DomainFamily::of_host("example.org");
    cx.update(|cx| {
      cx.set_global(AppSettings(Settings::default()));
      cx.set_global(SettingsStore::new(Some(path.clone())));
      SettingsStore::update(cx, |settings| settings.allow_family(&family));
    });
    let sender = watch_settings_for_test(path.clone(), cx);
    fs::write(&path, "allow_remote = true\n").unwrap();
    sender.try_send(()).unwrap();
    cx.executor().advance_clock(Duration::from_millis(150));
    cx.run_until_parked();

    let settings = Settings::load(&path).unwrap();
    assert!(settings.allowed_domains.iter().any(|domain| domain == "example.org"));
    assert!(cx.read_global::<AppSettings, _>(|settings, _| {
      settings.0.allowed_domains.iter().any(|domain| domain == "example.org")
    }));
  }

  #[cfg(unix)]
  #[gpui_kit::test]
  fn a_failed_settings_write_is_visible_and_clears_after_success(cx: &TestAppContext) {
    use std::os::unix::fs::PermissionsExt as _;

    cx.update(gpui_kit::init);
    let dir = tempfile::tempdir().unwrap();
    let read_only = dir.path().join("read-only");
    fs::create_dir(&read_only).unwrap();
    let path = read_only.join("settings.toml");
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&read_only, fs::Permissions::from_mode(0o555)).unwrap();
    cx.update(|cx| {
      cx.set_global(AppSettings(Settings::default()));
      cx.set_global(SettingsStore::new(Some(path.clone())));
      SettingsStore::update(cx, |settings| settings.allow_remote = true);
    });
    cx.run_until_parked();

    assert!(cx.read_global::<SettingsStore, _>(|store, _| store.last_error().is_some()));

    fs::set_permissions(&read_only, fs::Permissions::from_mode(0o755)).unwrap();
    fs::remove_dir(&path).unwrap();
    cx.update(|cx| SettingsStore::update(cx, |settings| settings.allow_remote = true));
    cx.run_until_parked();

    assert!(cx.read_global::<SettingsStore, _>(|store, _| store.last_error().is_none()));
    assert!(Settings::load(&path).unwrap().allow_remote);
  }
}
