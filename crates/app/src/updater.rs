//! Check GitHub Releases for a newer packaged build and install it.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use cargo_packager_updater::{Config, Update, UpdaterBuilder};
use futures::StreamExt as _;
use futures::channel::mpsc;
use futures::future::Either;
use gpui_kit::{App, AppContext, BorrowAppContext, Global};
use url::Url;

/// Manifest URL for the latest packaged release.
pub(crate) const UPDATER_ENDPOINT: &str = "https://github.com/felipefdl/openit/releases/latest/download/latest.json";
/// Minisign public key that verifies updater bundles.
pub(crate) const UPDATER_PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDdBQThERTBCQkQ4M0EzNjkKUldScG80TzlDOTZvZXF4anhXdzhMYi9RK0xMQWcrWVBoT2MzMlNTRXBpVFhxaHY1WmNiaFdYY0oK";

pub(crate) const CHECK_DELAY: Duration = Duration::from_secs(2);
pub(crate) const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const DOWNLOAD_TIMEOUT: Duration = Duration::from_mins(10);

/// Result of asking the updater whether a newer package exists.
#[derive(Clone)]
pub(crate) enum UpdateCheck {
  /// The running version matches the published manifest.
  UpToDate,
  /// A newer package is ready to download.
  Available { version: String, update: Box<Update> },
  /// The check did not complete.
  Failed(String),
}

enum UpdateStatus {
  Idle,
  Checking,
  UpToDate,
  Available {
    version: String,
    update: Box<Update>,
  },
  Downloading {
    version: String,
    percent: u8,
    update: Box<Update>,
  },
  Failed(String),
}

/// Check and install operations. Tests replace this with a fake.
pub(crate) trait UpdaterOps: Send + Sync {
  fn check(&self, current: &str) -> UpdateCheck;
  fn install(&self, update: Box<Update>, on_progress: &(dyn Fn(u8) + Send + Sync)) -> Result<(), String>;
}

struct LiveOps;

impl UpdaterOps for LiveOps {
  fn check(&self, current: &str) -> UpdateCheck {
    check_sync(current)
  }

  fn install(&self, update: Box<Update>, on_progress: &(dyn Fn(u8) + Send + Sync)) -> Result<(), String> {
    install_sync(*update, on_progress)
  }
}

/// In-flight check, last result, and download progress shared by every window.
pub(crate) struct UpdaterState {
  auto_started: bool,
  status: UpdateStatus,
  install_error: Option<String>,
  ops: Arc<dyn UpdaterOps>,
}

impl Default for UpdaterState {
  fn default() -> Self {
    Self {
      auto_started: false,
      status: UpdateStatus::Idle,
      install_error: None,
      ops: Arc::new(LiveOps),
    }
  }
}

impl Global for UpdaterState {}

impl UpdaterState {
  fn ops(&self) -> Arc<dyn UpdaterOps> {
    self.ops.clone()
  }

  fn begin_check(&mut self) -> bool {
    match self.status {
      UpdateStatus::Checking | UpdateStatus::Downloading { .. } => false,
      _ => {
        self.status = UpdateStatus::Checking;
        self.install_error = None;
        true
      },
    }
  }

  fn begin_auto_check(&mut self) -> bool {
    if self.auto_started {
      return false;
    }
    self.auto_started = true;
    self.begin_check()
  }

  fn apply_check(&mut self, result: UpdateCheck) {
    self.install_error = None;
    self.status = match result {
      UpdateCheck::Available { version, update } => UpdateStatus::Available { version, update },
      UpdateCheck::UpToDate => UpdateStatus::UpToDate,
      UpdateCheck::Failed(err) => UpdateStatus::Failed(err),
    };
  }

  fn begin_install(&mut self) -> Option<Box<Update>> {
    match &self.status {
      UpdateStatus::Available { version, update } => {
        let version = version.clone();
        let update = update.clone();
        self.install_error = None;
        self.status = UpdateStatus::Downloading {
          version,
          percent: 0,
          update: update.clone(),
        };
        Some(update)
      },
      _ => None,
    }
  }

  const fn set_percent(&mut self, percent: u8) {
    if let UpdateStatus::Downloading { percent: slot, .. } = &mut self.status {
      *slot = percent;
    }
  }

  fn finish_install(&mut self, result: Result<(), String>) {
    match result {
      Ok(()) => {},
      Err(err) => {
        if let UpdateStatus::Downloading { version, update, .. } =
          std::mem::replace(&mut self.status, UpdateStatus::Idle)
        {
          self.status = UpdateStatus::Available { version, update };
        }
        self.install_error = Some(err);
      },
    }
  }

  const fn can_install(&self) -> bool {
    matches!(self.status, UpdateStatus::Available { .. })
  }

  fn headline(&self) -> String {
    match &self.status {
      UpdateStatus::Idle | UpdateStatus::Checking => "Checking for updates...".to_owned(),
      UpdateStatus::UpToDate => format!("OpenIt {} is up to date", current_version()),
      UpdateStatus::Available { version, .. } => format!("OpenIt {version} is available"),
      UpdateStatus::Downloading { percent, .. } => format!("Installing {percent}%"),
      UpdateStatus::Failed(_) => "Could not check for updates".to_owned(),
    }
  }

  fn detail(&self) -> String {
    match &self.status {
      UpdateStatus::Failed(err) => err.clone(),
      _ => self.install_error.clone().unwrap_or_default(),
    }
  }

  #[cfg(test)]
  fn set_ops(&mut self, ops: Arc<dyn UpdaterOps>) {
    self.ops = ops;
  }
}

/// Packaged builds check on launch only when the user opted in.
pub(crate) const fn should_auto_check(debug: bool, opted_in: bool) -> bool {
  opted_in && !debug
}

pub(crate) const fn current_version() -> &'static str {
  env!("CARGO_PKG_VERSION")
}

pub(crate) fn download_percent(received: u64, total: Option<u64>) -> u8 {
  match total {
    Some(total) if total > 0 => u8::try_from((received.saturating_mul(100) / total).min(100)).unwrap_or(u8::MAX),
    _ => 0,
  }
}

/// Open the Updates modal and start a check.
#[allow(
  clippy::needless_pass_by_ref_mut,
  reason = "GPUI action handlers receive a mutable App"
)]
pub(crate) fn check_from_menu(cx: &mut App) {
  ensure_state(cx);
  // Menu dispatch still holds the host window until the action returns.
  cx.defer(open_dialog);
  start_check(cx, false);
}

/// Wait briefly after launch, then check if the setting is still on.
pub(crate) fn schedule_auto_check(cx: &mut App) {
  ensure_state(cx);
  cx.spawn(async move |cx| {
    cx.background_executor().timer(CHECK_DELAY).await;
    cx.update(start_auto_check);
  })
  .detach();
}

fn start_auto_check(cx: &mut App) {
  if !cx.global::<crate::settings::AppSettings>().0.auto_check_updates {
    return;
  }
  start_check(cx, true);
}

fn start_check(cx: &mut App, automatic: bool) {
  let started = cx.update_global::<UpdaterState, _>(|state, _| {
    if automatic {
      state.begin_auto_check()
    } else {
      state.begin_check()
    }
  });
  if !started {
    return;
  }
  let current = current_version().to_owned();
  let ops = take_ops(cx);
  cx.spawn(async move |cx| {
    let result = cx.background_spawn(async move { ops.check(&current) }).await;
    cx.update(|cx| {
      let available = matches!(result, UpdateCheck::Available { .. });
      if automatic
        && !available
        && let UpdateCheck::Failed(error) = &result
      {
        tracing::warn!(%error, "automatic update check failed");
      }
      cx.update_global::<UpdaterState, _>(|state, _| state.apply_check(result));
      if available && automatic {
        open_dialog(cx);
      }
    });
  })
  .detach();
}

fn start_install(cx: &mut App) {
  let Some(update) = cx.update_global::<UpdaterState, _>(|state, _| state.begin_install()) else {
    return;
  };
  let ops = take_ops(cx);
  let (progress_tx, progress_rx) = mpsc::unbounded();
  cx.spawn(async move |cx| {
    let task = cx.background_spawn(async move {
      let on_progress = move |percent: u8| {
        let _ = progress_tx.unbounded_send(percent);
      };
      ops.install(update, &on_progress)
    });
    let result = watch_install(task, progress_rx, |percent| {
      cx.update(|cx| {
        cx.update_global::<UpdaterState, _>(|state, _| state.set_percent(percent));
      });
    })
    .await;
    cx.update(|cx| {
      cx.update_global::<UpdaterState, _>(|state, _| state.finish_install(result));
    });
  })
  .detach();
}

async fn watch_install<T>(
  task: impl Future<Output = T>,
  mut rx: mpsc::UnboundedReceiver<u8>,
  mut on_percent: impl FnMut(u8),
) -> T {
  let mut task = std::pin::pin!(task);
  loop {
    match futures::future::select(rx.next(), task.as_mut()).await {
      Either::Left((Some(percent), _)) => on_percent(percent),
      Either::Left((None, rest)) => return rest.await,
      Either::Right((result, _)) => return result,
    }
  }
}

fn ensure_state(cx: &mut App) {
  if !cx.has_global::<UpdaterState>() {
    cx.set_global(UpdaterState::default());
  }
}

fn take_ops(cx: &mut App) -> Arc<dyn UpdaterOps> {
  ensure_state(cx);
  cx.global::<UpdaterState>().ops()
}

#[derive(Default)]
struct UpdateDialog {
  active: Option<(gpui_kit::AnyWindowHandle, gpui_kit::WeakEntity<dialog::UpdateView>)>,
}

impl Global for UpdateDialog {}

fn open_dialog(cx: &mut App) {
  if let Some((handle, view)) = cx
    .try_global::<UpdateDialog>()
    .and_then(|state| state.active.as_ref())
    .and_then(|(handle, view)| view.upgrade().map(|view| (*handle, view)))
    && handle
      .update(cx, |_, window, cx| {
        view.update(cx, |view, cx| view.focus(window, cx));
        window.activate_window();
      })
      .is_ok()
  {
    return;
  }
  if let Some(handle) = cx.active_window()
    && show_dialog_in(handle, cx)
  {
    return;
  }
  for handle in cx.windows() {
    if show_dialog_in(handle, cx) {
      return;
    }
  }
  crate::window::open_empty_window(cx);
  for handle in cx.windows() {
    if show_dialog_in(handle, cx) {
      return;
    }
  }
}

fn show_dialog_in(handle: gpui_kit::AnyWindowHandle, cx: &mut App) -> bool {
  handle
    .update(cx, |_, window, cx| {
      if let Some(view) = window.root::<crate::document_view::DocumentView>().flatten() {
        view.update(cx, |view, cx| view.open_updates(window, cx));
      } else if let Some(view) = window.root::<crate::empty_view::EmptyView>().flatten() {
        view.update(cx, |view, cx| view.open_updates(window, cx));
      } else if let Some(view) = window.root::<crate::image_view::ImageView>().flatten() {
        view.update(cx, |view, cx| view.open_updates(window, cx));
      } else if let Some(view) = window.root::<crate::pdf_view::PdfView>().flatten() {
        view.update(cx, |view, cx| view.open_updates(window, cx));
      } else {
        return false;
      }
      window.activate_window();
      true
    })
    .unwrap_or(false)
}

fn updater_config() -> Result<Config, String> {
  let endpoint: Url = UPDATER_ENDPOINT.parse().map_err(|err: url::ParseError| err.to_string())?;
  Ok(Config {
    endpoints: vec![endpoint],
    pubkey: UPDATER_PUBKEY.into(),
    windows: None,
  })
}

fn check_sync(current: &str) -> UpdateCheck {
  let version = match current.parse::<cargo_packager_updater::semver::Version>() {
    Ok(version) => version,
    Err(err) => return UpdateCheck::Failed(err.to_string()),
  };
  let config = match updater_config() {
    Ok(config) => config,
    Err(err) => return UpdateCheck::Failed(err),
  };
  match UpdaterBuilder::new(version, config).timeout(CHECK_TIMEOUT).build() {
    Ok(updater) => match updater.check() {
      Ok(Some(update)) => UpdateCheck::Available {
        version: update.version.clone(),
        update: Box::new(update),
      },
      Ok(None) => UpdateCheck::UpToDate,
      Err(err) => UpdateCheck::Failed(err.to_string()),
    },
    Err(err) => UpdateCheck::Failed(err.to_string()),
  }
}

fn relaunch(update: &Update) -> Result<(), String> {
  #[cfg(windows)]
  {
    let _ = update;
    Ok(())
  }
  #[cfg(target_os = "macos")]
  {
    std::process::Command::new("open")
      .arg("-n")
      .arg(&update.extract_path)
      .spawn()
      .map_err(|err| err.to_string())?;
    std::process::exit(0);
  }
  #[cfg(not(any(windows, target_os = "macos")))]
  {
    std::process::Command::new(&update.extract_path)
      .spawn()
      .map_err(|err| err.to_string())?;
    std::process::exit(0);
  }
}

fn install_sync(mut update: Update, on_progress: &(dyn Fn(u8) + Send + Sync)) -> Result<(), String> {
  install_package(&mut update, on_progress)?;
  relaunch(&update)
}

fn install_package(update: &mut Update, on_progress: &(dyn Fn(u8) + Send + Sync)) -> Result<(), String> {
  update.timeout = Some(DOWNLOAD_TIMEOUT);
  let received = AtomicU64::new(0);
  update
    .download_and_install_extended(
      |chunk, total| {
        let added = u64::try_from(chunk).unwrap_or(u64::MAX);
        let previous = received.fetch_add(added, Ordering::Relaxed);
        on_progress(download_percent(previous.saturating_add(added), total));
      },
      || {},
    )
    .map_err(|err| err.to_string())
}

pub(crate) mod dialog {
  use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
  use gpui_kit::component::checkbox::Checkbox;
  use gpui_kit::component::input::Escape;
  use gpui_kit::component::{ActiveTheme, Disableable, Sizable};
  use gpui_kit::prelude::*;
  use gpui_kit::{
    App, Context, EventEmitter, FocusHandle, FontWeight, IntoElement, Render, Subscription, Window, div, px, svg,
  };

  use super::{UpdateDialog, UpdaterState, current_version, start_install};
  use crate::actions::CloseWindow;
  use crate::settings::{AppSettings, SettingsStore};
  use crate::status_pickers::overlay_frame;
  use crate::theme::ActivePalette;

  pub(crate) enum UpdateEvent {
    Close,
  }

  pub(crate) struct UpdateView {
    focus: FocusHandle,
    _state: Subscription,
    _settings: Subscription,
  }

  impl EventEmitter<UpdateEvent> for UpdateView {}

  impl UpdateView {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
      let focus = cx.focus_handle();
      window.focus(&focus, cx);
      let view = cx.entity().downgrade();
      cx.default_global::<UpdateDialog>().active = Some((window.window_handle(), view));
      Self {
        focus,
        _state: cx.observe_global::<UpdaterState>(|_, cx| cx.notify()),
        _settings: cx.observe_global::<AppSettings>(|_, cx| cx.notify()),
      }
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
      window.focus(&self.focus, cx);
    }

    #[expect(clippy::unused_self, reason = "CloseWindow listener signature")]
    fn close(&mut self, _: &CloseWindow, _: &mut Window, cx: &mut Context<Self>) {
      cx.emit(UpdateEvent::Close);
    }

    #[expect(clippy::unused_self, reason = "Escape listener signature")]
    fn escape(&mut self, _: &Escape, _: &mut Window, cx: &mut Context<Self>) {
      cx.emit(UpdateEvent::Close);
    }

    #[expect(clippy::unused_self, reason = "Cancel button listener signature")]
    fn cancel(&mut self, _: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
      cx.emit(UpdateEvent::Close);
    }

    #[expect(clippy::unused_self, reason = "Install button listener signature")]
    fn install(&mut self, _: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
      start_install(cx);
    }

    #[expect(clippy::unused_self, reason = "checkbox listener signature")]
    #[expect(clippy::needless_pass_by_ref_mut, reason = "checkbox listener signature")]
    pub(crate) fn set_auto_check(&mut self, value: bool, cx: &mut Context<Self>) {
      SettingsStore::update(cx, |settings| settings.auto_check_updates = value);
    }

    #[expect(clippy::unused_self, reason = "footer is rendered from the view")]
    #[expect(
      clippy::needless_pass_by_ref_mut,
      reason = "cx.listener requires the mutable Context signature"
    )]
    fn footer(&self, can_install: bool, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
      div()
        .flex()
        .items_center()
        .justify_end()
        .flex_shrink_0()
        .gap_2()
        .child(
          Button::new("updates-close")
            .label("Close")
            .small()
            .on_click(cx.listener(Self::cancel)),
        )
        .child(
          Button::new("updates-install")
            .label("Install Update")
            .with_variant(ButtonVariant::Primary)
            .small()
            .disabled(!can_install)
            .on_click(cx.listener(Self::install)),
        )
        .into_any_element()
    }
  }

  impl Render for UpdateView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
      let theme = cx.theme();
      let background = theme.background;
      let palette = cx.global::<ActivePalette>().0;
      let foreground = theme.foreground;
      let muted = theme.muted_foreground;
      let font = theme.font_family.clone();
      let mark = cx.global::<ActivePalette>().mark();
      let state = cx.global::<UpdaterState>();
      let headline = state.headline();
      let detail = state.detail();
      let can_install = state.can_install();
      let auto_check = cx.global::<AppSettings>().0.auto_check_updates;
      let content = div()
        .flex()
        .flex_col()
        .size_full()
        .p_6()
        .gap_5()
        .text_sm()
        .text_color(foreground)
        .font_family(font)
        .child(
          div()
            .flex()
            .items_center()
            .gap_4()
            .flex_shrink_0()
            .child(
              svg()
                .path("brand/openit-glyph.svg")
                .size(px(48.))
                .text_color(mark)
                .flex_shrink_0(),
            )
            .child(
              div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_base().font_weight(FontWeight::SEMIBOLD).child("OpenIt"))
                .child(
                  div()
                    .text_xs()
                    .text_color(muted)
                    .child(format!("Version {}", current_version())),
                ),
            ),
        )
        .child(
          div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_2()
            .child(div().flex_shrink_0().font_weight(FontWeight::SEMIBOLD).child(headline))
            .child(
              div()
                .id("updates-detail")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .text_xs()
                .text_color(muted)
                .child(detail),
            ),
        )
        .child(
          div().flex_shrink_0().child(
            Checkbox::new("auto-check-updates")
              .small()
              .label("Automatically check for updates")
              .checked(auto_check)
              .on_click(cx.listener(|this, value: &bool, _, cx| this.set_auto_check(*value, cx))),
          ),
        )
        .child(self.footer(can_install, cx));
      let view = cx.entity().downgrade();
      overlay_frame(
        "updates",
        &palette,
        move |_, _, cx| {
          let _ = view.update(cx, |_, cx| cx.emit(UpdateEvent::Close));
        },
        |panel| panel.mt_0().w(px(460.)).h(px(300.)).max_w_full().max_h_full().child(content),
      )
      .items_center()
      .key_context("Updates")
      .bg(background.opacity(0.65))
      .p_4()
      .track_focus(&self.focus)
      .on_action(cx.listener(Self::close))
      .on_action(cx.listener(Self::escape))
    }
  }
}

#[cfg(test)]
#[path = "updater_tests.rs"]
mod integration_tests;

#[cfg(test)]
pub(crate) struct FakeOps {
  pub check: UpdateCheck,
  pub percents: Vec<u8>,
  pub install: Result<(), String>,
  pub checks: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
pub(crate) fn dummy_update(version: &str) -> Box<Update> {
  Box::new(Update {
    config: Config {
      endpoints: Vec::new(),
      pubkey: String::new(),
      windows: None,
    },
    body: None,
    current_version: "0.2.1".into(),
    version: version.into(),
    date: None,
    target: "macos".into(),
    extract_path: std::path::PathBuf::new(),
    download_url: "https://example.com/OpenIt.app.tar.gz".parse().expect("url"),
    signature: String::new(),
    timeout: None,
    headers: cargo_packager_updater::http::HeaderMap::new(),
    format: cargo_packager_updater::UpdateFormat::App,
  })
}

#[cfg(test)]
impl FakeOps {
  pub fn available() -> Self {
    Self {
      check: UpdateCheck::Available {
        version: "0.3.0".into(),
        update: dummy_update("0.3.0"),
      },
      percents: vec![40],
      install: Ok(()),
      checks: std::sync::atomic::AtomicUsize::new(0),
    }
  }
}

#[cfg(test)]
impl UpdaterOps for FakeOps {
  fn check(&self, _current: &str) -> UpdateCheck {
    self.checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    self.check.clone()
  }

  fn install(&self, _update: Box<Update>, on_progress: &(dyn Fn(u8) + Send + Sync)) -> Result<(), String> {
    for percent in &self.percents {
      on_progress(*percent);
    }
    self.install.clone()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use cargo_packager_updater::{RemoteRelease, RemoteReleaseData};
  use gpui_kit::TestAppContext;
  use openit_core::settings::Settings;

  use crate::settings::{AppSettings, SettingsStore};
  use crate::theme::ThemeDirs;

  fn init_updater(cx: &TestAppContext, ops: FakeOps) {
    cx.update(|cx| {
      gpui_kit::init(cx);
      cx.set_global(AppSettings(Settings::default()));
      cx.set_global(SettingsStore::default());
      cx.set_global(ThemeDirs::default());
      crate::theme::init(cx);
      let mut state = UpdaterState::default();
      state.set_ops(Arc::new(ops));
      cx.set_global(state);
    });
  }

  fn active_dialog(cx: &App) -> Option<gpui_kit::Entity<dialog::UpdateView>> {
    cx.try_global::<UpdateDialog>()
      .and_then(|state| state.active.as_ref())
      .and_then(|(_, view)| view.upgrade())
  }

  #[test]
  fn should_auto_check_requires_opt_in_and_skips_debug() {
    assert!(!should_auto_check(true, true));
    assert!(!should_auto_check(false, false));
    assert!(should_auto_check(false, true));
  }

  #[test]
  fn in_flight_check_is_ignored() {
    let mut state = UpdaterState::default();
    assert!(state.begin_check());
    assert!(!state.begin_check());
  }

  #[test]
  fn auto_check_runs_once() {
    let mut state = UpdaterState::default();
    assert!(state.begin_auto_check());
    state.apply_check(UpdateCheck::UpToDate);
    assert!(!state.begin_auto_check());
  }

  #[test]
  fn failed_check_keeps_the_error() {
    let mut state = UpdaterState::default();
    state.apply_check(UpdateCheck::Failed("operation timed out".into()));
    assert_eq!(state.detail(), "operation timed out");
    assert!(!state.can_install());
  }

  #[test]
  fn timed_out_download_restores_available() {
    let mut state = UpdaterState::default();
    state.apply_check(UpdateCheck::Available {
      version: "0.3.0".into(),
      update: dummy_update("0.3.0"),
    });
    assert!(state.begin_install().is_some());
    state.set_percent(40);
    state.finish_install(Err("operation timed out".into()));
    assert!(state.can_install());
    assert_eq!(state.detail(), "operation timed out");
  }

  #[test]
  fn manifest_parses_sample() {
    let json = r#"{
      "version": "v0.3.0",
      "notes": "Test version",
      "pub_date": "2020-06-22T19:25:57Z",
      "platforms": {
        "macos-aarch64": {
          "signature": "Content of app.tar.gz.sig",
          "url": "https://github.com/felipefdl/openit/releases/download/v0.3.0/OpenIt_0.3.0_aarch64.app.tar.gz",
          "format": "app"
        },
        "linux-x86_64": {
          "signature": "Content of app.AppImage.sig",
          "url": "https://github.com/felipefdl/openit/releases/download/v0.3.0/OpenIt_0.3.0_amd64.AppImage",
          "format": "appimage"
        },
        "windows-x86_64": {
          "signature": "Content of app-setup.exe.sig",
          "url": "https://github.com/felipefdl/openit/releases/download/v0.3.0/OpenIt_0.3.0_x64-setup.exe",
          "format": "nsis"
        }
      }
    }"#;
    let release: RemoteRelease = serde_json::from_str(json).expect("sample latest.json");
    assert_eq!(release.version.to_string(), "0.3.0");
    assert_eq!(release.notes.as_deref(), Some("Test version"));
    assert!(release.pub_date.is_some());
    match release.data {
      RemoteReleaseData::Static { platforms } => {
        assert!(platforms.contains_key("macos-aarch64"));
        assert!(platforms.contains_key("linux-x86_64"));
        assert!(platforms.contains_key("windows-x86_64"));
      },
      RemoteReleaseData::Dynamic(_) => panic!("expected platforms map"),
    }
  }

  #[test]
  fn download_percent_uses_content_length() {
    assert_eq!(download_percent(0, Some(100)), 0);
    assert_eq!(download_percent(50, Some(200)), 25);
    assert_eq!(download_percent(200, Some(200)), 100);
    assert_eq!(download_percent(10, None), 0);
    assert_eq!(download_percent(10, Some(0)), 0);
  }

  #[gpui_kit::test]
  fn menu_check_reuses_the_host_and_dismissal_restores_focus(cx: &mut TestAppContext) {
    init_updater(cx, FakeOps::available());
    cx.update(|cx| {
      cx.on_action(|_: &crate::actions::CheckForUpdates, cx| check_from_menu(cx));
    });
    cx.update(crate::window::open_empty_window);
    let host = cx.windows()[0];
    let original_focus = host.update(cx, |_, window, cx| window.focused(cx)).unwrap();
    cx.dispatch_action(host, crate::actions::CheckForUpdates);
    cx.run_until_parked();
    let view = cx.update(|cx| {
      assert!(cx.global::<UpdaterState>().can_install());
      active_dialog(cx).expect("Updates modal")
    });
    cx.dispatch_action(host, crate::actions::CheckForUpdates);
    cx.run_until_parked();
    cx.update(|cx| {
      assert_eq!(cx.windows(), vec![host]);
      assert_eq!(active_dialog(cx), Some(view.clone()), "reopening focuses the same modal");
      view.update(cx, |_, cx| cx.emit(dialog::UpdateEvent::Close));
    });
    drop(view);
    cx.run_until_parked();
    assert!(cx.update(|cx| active_dialog(cx).is_none()));
    assert_eq!(cx.windows(), vec![host], "closing the modal preserves its host");
    assert_eq!(host.update(cx, |_, window, cx| window.focused(cx)).unwrap(), original_focus);
  }

  #[gpui_kit::test]
  #[expect(clippy::needless_pass_by_ref_mut, reason = "gpui-kit test harness")]
  fn automatic_check_stays_quiet_when_up_to_date(cx: &mut TestAppContext) {
    let mut ops = FakeOps::available();
    ops.check = UpdateCheck::UpToDate;
    init_updater(cx, ops);
    cx.update(|cx| {
      SettingsStore::update(cx, |settings| settings.auto_check_updates = true);
      start_auto_check(cx);
    });
    cx.run_until_parked();
    cx.update(|cx| {
      assert!(!cx.global::<UpdaterState>().can_install());
      assert!(active_dialog(cx).is_none());
      assert!(cx.windows().is_empty());
    });
  }

  #[gpui_kit::test]
  #[expect(clippy::needless_pass_by_ref_mut, reason = "gpui-kit test harness")]
  fn automatic_check_opens_the_dialog_when_an_update_exists(cx: &mut TestAppContext) {
    init_updater(cx, FakeOps::available());
    cx.update(|cx| {
      SettingsStore::update(cx, |settings| settings.auto_check_updates = true);
      start_auto_check(cx);
    });
    cx.run_until_parked();
    cx.update(|cx| {
      assert!(cx.global::<UpdaterState>().can_install());
      assert!(active_dialog(cx).is_some());
      assert_eq!(cx.windows().len(), 1);
      assert!(cx.windows()[0].downcast::<crate::empty_view::EmptyView>().is_some());
    });
  }

  #[gpui_kit::test]
  #[expect(clippy::needless_pass_by_ref_mut, reason = "gpui-kit test harness")]
  fn checkbox_writes_auto_check_updates(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    init_updater(cx, FakeOps::available());
    cx.update(|cx| SettingsStore::set_path(cx, path.clone()));
    cx.update(open_dialog);
    cx.update(|cx| {
      active_dialog(cx)
        .expect("Updates modal")
        .update(cx, |view, cx| view.set_auto_check(true, cx));
    });
    cx.run_until_parked();
    assert!(Settings::load(&path).unwrap().auto_check_updates);
  }
}
