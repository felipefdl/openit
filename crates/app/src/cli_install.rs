//! Install and remove the `openit` and `oi` command links.
#![cfg_attr(
  not(target_os = "macos"),
  expect(
    dead_code,
    reason = "command-line tool links are installed from the macOS window"
  )
)]

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const OPENIT: &str = "openit";
const OI: &str = "oi";
const SUCCESS: &str = "Open a new terminal to use them.";

/// Whether each command name is present in an install directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LinkStatus {
  openit: bool,
  oi: bool,
}

/// Names to create and remove so the directory matches the checkboxes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
  create: Vec<&'static str>,
  remove: Vec<&'static str>,
}

/// Why an install or remove failed.
#[derive(Debug)]
pub(crate) enum InstallError {
  /// The administrator prompt was dismissed.
  Cancelled,
  /// A named filesystem or process failure.
  Message(String),
}

impl fmt::Display for InstallError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Cancelled => write!(f, "Authorization cancelled"),
      Self::Message(message) => f.write_str(message),
    }
  }
}

impl std::error::Error for InstallError {}

impl InstallError {
  const fn is_cancelled(&self) -> bool {
    matches!(self, Self::Cancelled)
  }
}

/// `/usr/local/bin` on Unix, where the macOS modal writes.
pub(crate) fn unix_install_dir() -> PathBuf {
  PathBuf::from("/usr/local/bin")
}

/// Presence of `openit` and `oi` in `dir` (files or dangling links count).
pub(crate) fn status_in(dir: &Path) -> LinkStatus {
  LinkStatus {
    openit: present(&dir.join(OPENIT)),
    oi: present(&dir.join(OI)),
  }
}

/// Checkbox defaults: both on when nothing is installed, otherwise the current state.
pub(crate) const fn initial_wanted(installed: LinkStatus) -> LinkStatus {
  if !installed.openit && !installed.oi {
    LinkStatus { openit: true, oi: true }
  } else {
    installed
  }
}

/// Names to create and remove so `installed` becomes `wanted`.
pub(crate) fn plan(installed: LinkStatus, wanted: LinkStatus) -> Plan {
  let mut create = Vec::new();
  let mut remove = Vec::new();
  for (name, have, want) in [(OPENIT, installed.openit, wanted.openit), (OI, installed.oi, wanted.oi)] {
    if want && !have {
      create.push(name);
    } else if have && !want {
      remove.push(name);
    }
  }
  Plan { create, remove }
}

/// Link target: `$APPIMAGE` when set, otherwise `current_exe` resolved through symlinks.
pub(crate) fn resolve_link_target(current_exe: &Path, appimage: Option<&str>) -> PathBuf {
  match appimage {
    Some(path) if !path.is_empty() => PathBuf::from(path),
    _ => fs::canonicalize(current_exe).unwrap_or_else(|_| current_exe.to_path_buf()),
  }
}

/// Resolve the running binary, preferring `$APPIMAGE`.
pub(crate) fn link_target() -> Result<PathBuf, InstallError> {
  let exe = std::env::current_exe()
    .map_err(|error| InstallError::Message(format!("could not locate the OpenIt binary: {error}")))?;
  Ok(resolve_link_target(&exe, std::env::var("APPIMAGE").ok().as_deref()))
}

/// Write or delete the planned links in `dir`.
#[cfg(unix)]
pub(crate) fn apply_in(dir: &Path, target: &Path, planned: &Plan) -> Result<(), InstallError> {
  if dir_is_writable(dir) {
    apply_direct(dir, target, planned)
  } else {
    #[cfg(target_os = "macos")]
    {
      apply_elevated(dir, target, planned)
    }
    #[cfg(not(target_os = "macos"))]
    {
      Err(InstallError::Message(format!("{} is not writable", dir.display())))
    }
  }
}

/// A `cmd.exe` shim that forwards every argument to `exe`.
#[cfg_attr(not(test), expect(dead_code, reason = "kept for a later NSIS install step"))]
pub(crate) fn windows_shim(exe: &Path) -> String {
  let path = exe.display().to_string().replace('"', "\"\"");
  format!("@echo off\r\n\"{path}\" %*\r\n")
}

/// User PATH with `dir` appended when it is not already listed.
#[cfg_attr(not(test), expect(dead_code, reason = "kept for a later NSIS install step"))]
pub(crate) fn windows_path_adding(current: &str, dir: &str) -> String {
  if current.split(';').any(|entry| entry.eq_ignore_ascii_case(dir)) {
    current.to_owned()
  } else if current.is_empty() {
    dir.to_owned()
  } else {
    format!("{current};{dir}")
  }
}

/// User PATH with `dir` removed.
#[cfg_attr(not(test), expect(dead_code, reason = "kept for a later NSIS install step"))]
pub(crate) fn windows_path_removing(current: &str, dir: &str) -> String {
  current
    .split(';')
    .filter(|entry| !entry.eq_ignore_ascii_case(dir))
    .collect::<Vec<_>>()
    .join(";")
}

fn present(path: &Path) -> bool {
  path.symlink_metadata().is_ok()
}

fn io_fail(verb: &str, path: &Path, error: &io::Error) -> InstallError {
  InstallError::Message(format!("could not {verb} {}: {error}", path.display()))
}

#[cfg(unix)]
fn dir_is_writable(dir: &Path) -> bool {
  if !dir.is_dir() {
    return false;
  }
  let probe = dir.join(".openit-write-test");
  if fs::write(&probe, []).is_ok() {
    let _ = fs::remove_file(&probe);
    true
  } else {
    false
  }
}

#[cfg(unix)]
fn apply_direct(dir: &Path, target: &Path, planned: &Plan) -> Result<(), InstallError> {
  for name in &planned.remove {
    remove_link(&dir.join(name))?;
  }
  for name in &planned.create {
    create_link(&dir.join(name), target)?;
  }
  Ok(())
}

#[cfg(unix)]
fn create_link(link: &Path, target: &Path) -> Result<(), InstallError> {
  if present(link) {
    fs::remove_file(link).map_err(|error| io_fail("remove", link, &error))?;
  }
  std::os::unix::fs::symlink(target, link).map_err(|error| io_fail("create", link, &error))
}

#[cfg(unix)]
fn remove_link(link: &Path) -> Result<(), InstallError> {
  if present(link) {
    fs::remove_file(link).map_err(|error| io_fail("remove", link, &error))?;
  }
  Ok(())
}

#[cfg(target_os = "macos")]
fn apply_elevated(dir: &Path, target: &Path, planned: &Plan) -> Result<(), InstallError> {
  let mut cmds = vec![format!("mkdir -p {}", sh_quote(&dir.to_string_lossy()))];
  for name in &planned.remove {
    cmds.push(format!("rm -f {}", sh_quote(&dir.join(name).to_string_lossy())));
  }
  for name in &planned.create {
    let link = dir.join(name);
    cmds.push(format!("rm -f {}", sh_quote(&link.to_string_lossy())));
    cmds.push(format!(
      "ln -s {} {}",
      sh_quote(&target.to_string_lossy()),
      sh_quote(&link.to_string_lossy())
    ));
  }
  run_osascript(&cmds.join(" && "))
}

#[cfg(target_os = "macos")]
fn sh_quote(value: &str) -> String {
  format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(target_os = "macos")]
fn cancelled(stderr: &str) -> bool {
  stderr.contains("User canceled") || stderr.contains("User cancelled") || stderr.contains("-128")
}

#[cfg(target_os = "macos")]
fn run_osascript(shell_cmd: &str) -> Result<(), InstallError> {
  let escaped = shell_cmd.replace('\\', "\\\\").replace('"', "\\\"");
  let script = format!("do shell script \"{escaped}\" with administrator privileges");
  let output = std::process::Command::new("osascript")
    .args(["-e", &script])
    .output()
    .map_err(|error| InstallError::Message(format!("could not prompt for administrator access: {error}")))?;
  if output.status.success() {
    return Ok(());
  }
  let stderr = String::from_utf8_lossy(&output.stderr);
  if cancelled(&stderr) {
    return Err(InstallError::Cancelled);
  }
  Err(InstallError::Message(format!("could not update the commands: {stderr}")))
}

/// Open the macOS install window.
#[cfg(target_os = "macos")]
pub(crate) fn open_window(cx: &mut gpui_kit::App) {
  dialog::open(cx);
}

#[cfg(target_os = "macos")]
mod dialog {
  use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
  use gpui_kit::component::checkbox::Checkbox;
  use gpui_kit::component::input::Escape;
  use gpui_kit::component::{ActiveTheme, Disableable, Sizable};
  use gpui_kit::prelude::*;
  use gpui_kit::{
    App, Bounds, Context, FocusHandle, IntoElement, Render, SharedString, TitlebarOptions, Window, WindowBounds,
    WindowOptions, div, px, size,
  };

  use super::{
    InstallError, LinkStatus, SUCCESS, apply_in, initial_wanted, link_target, plan, status_in, unix_install_dir,
  };
  use crate::actions::CloseWindow;
  use crate::status_pickers::overlay_frame;
  use crate::theme::ActivePalette;

  pub(super) fn open(cx: &mut App) {
    let options = WindowOptions {
      titlebar: Some(TitlebarOptions {
        title: Some("Install Command Line Tools".into()),
        appears_transparent: false,
        traffic_light_position: None,
      }),
      window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(420.), px(260.)), cx))),
      window_min_size: Some(size(px(420.), px(260.))),
      is_resizable: false,
      ..WindowOptions::default()
    };
    if let Err(error) = cx.open_window(options, |window, cx| cx.new(|cx| InstallView::new(window, cx))) {
      tracing::error!(%error, "install window open failed");
    }
  }

  struct InstallView {
    focus: FocusHandle,
    installed: LinkStatus,
    wanted: LinkStatus,
    status: String,
    applying: bool,
  }

  impl InstallView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
      let focus = cx.focus_handle();
      window.focus(&focus, cx);
      let installed = status_in(&unix_install_dir());
      Self {
        focus,
        installed,
        wanted: initial_wanted(installed),
        status: String::new(),
        applying: false,
      }
    }

    const fn dirty(&self) -> bool {
      self.wanted.openit != self.installed.openit || self.wanted.oi != self.installed.oi
    }

    #[expect(clippy::unused_self, reason = "CloseWindow listener signature")]
    fn close(&mut self, _: &CloseWindow, window: &mut Window, _: &mut Context<Self>) {
      window.remove_window();
    }

    #[expect(clippy::unused_self, reason = "Escape listener signature")]
    fn escape(&mut self, _: &Escape, window: &mut Window, _: &mut Context<Self>) {
      window.remove_window();
    }

    #[expect(clippy::unused_self, reason = "Cancel button listener signature")]
    fn cancel(&mut self, _: &gpui_kit::ClickEvent, window: &mut Window, _: &mut Context<Self>) {
      window.remove_window();
    }

    fn apply(&mut self, _: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
      if self.applying || !self.dirty() {
        return;
      }
      let target = match link_target() {
        Ok(target) => target,
        Err(error) => {
          self.status = error.to_string();
          cx.notify();
          return;
        },
      };
      self.applying = true;
      cx.notify();
      let planned = plan(self.installed, self.wanted);
      let dir = unix_install_dir();
      cx.spawn(async move |this, cx| {
        let result = cx.background_spawn(async move { apply_in(&dir, &target, &planned) }).await;
        let _ = this.update(cx, |this, cx| this.finish(result, cx));
      })
      .detach();
    }

    fn finish(&mut self, result: Result<(), InstallError>, cx: &mut Context<Self>) {
      self.applying = false;
      match result {
        Ok(()) => {
          self.installed = status_in(&unix_install_dir());
          self.wanted = self.installed;
          SUCCESS.clone_into(&mut self.status);
        },
        Err(error) if error.is_cancelled() => {},
        Err(error) => self.status = error.to_string(),
      }
      cx.notify();
    }

    fn set_openit(&mut self, value: bool, cx: &mut Context<Self>) {
      self.wanted.openit = value;
      cx.notify();
    }

    fn set_oi(&mut self, value: bool, cx: &mut Context<Self>) {
      self.wanted.oi = value;
      cx.notify();
    }

    fn example(example: &'static str, muted: gpui_kit::Hsla, mono: SharedString) -> gpui_kit::AnyElement {
      div()
        .pl_7()
        .text_xs()
        .font_family(mono)
        .text_color(muted)
        .child(example)
        .into_any_element()
    }

    #[expect(
      clippy::needless_pass_by_ref_mut,
      reason = "cx.listener requires the mutable Context signature"
    )]
    fn footer(&self, muted: gpui_kit::Hsla, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
      let border = cx.theme().border;
      let apply_off = !self.dirty() || self.applying;
      div()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_2()
        .border_t_1()
        .border_color(border)
        .child(
          div()
            .flex_1()
            .h(px(20.))
            .overflow_hidden()
            .text_xs()
            .text_color(muted)
            .child(self.status.clone()),
        )
        .child(
          Button::new("cli-install-cancel")
            .label("Cancel")
            .small()
            .on_click(cx.listener(Self::cancel)),
        )
        .child(
          Button::new("cli-install-apply")
            .label("Apply")
            .with_variant(ButtonVariant::Primary)
            .small()
            .disabled(apply_off)
            .on_click(cx.listener(Self::apply)),
        )
        .into_any_element()
    }
  }

  impl Render for InstallView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
      let palette = cx.global::<ActivePalette>().0;
      let muted = cx.theme().muted_foreground;
      let mono = cx.theme().mono_font_family.clone();
      let children = vec![
        div()
          .flex()
          .flex_col()
          .flex_1()
          .px_3()
          .pt_3()
          .gap_3()
          .child(
            div()
              .flex()
              .flex_col()
              .gap_1()
              .child(
                Checkbox::new("cli-install-openit")
                  .label("openit")
                  .checked(self.wanted.openit)
                  .on_click(cx.listener(|this, value: &bool, _, cx| this.set_openit(*value, cx))),
              )
              .child(Self::example("openit file.md", muted, mono.clone())),
          )
          .child(
            div()
              .flex()
              .flex_col()
              .gap_1()
              .child(
                Checkbox::new("cli-install-oi")
                  .label("oi")
                  .checked(self.wanted.oi)
                  .on_click(cx.listener(|this, value: &bool, _, cx| this.set_oi(*value, cx))),
              )
              .child(Self::example("oi file.md", muted, mono)),
          )
          .into_any_element(),
        self.footer(muted, cx),
      ];
      overlay_frame(
        "cli-install",
        &palette,
        |_, _, _| {},
        move |panel| {
          panel
            .mt(px(0.))
            .w_full()
            .h_full()
            .rounded_none()
            .shadow_none()
            .border_0()
            .children(children)
        },
      )
      .size_full()
      .key_context("Dialog")
      .track_focus(&self.focus)
      .on_action(cx.listener(Self::close))
      .on_action(cx.listener(Self::escape))
    }
  }
}

#[cfg(test)]
mod tests {
  use std::fs;
  use std::path::PathBuf;

  #[cfg(unix)]
  use super::apply_in;
  use super::{
    LinkStatus, Plan, initial_wanted, plan, resolve_link_target, status_in, windows_path_adding, windows_path_removing,
    windows_shim,
  };

  fn status(openit: bool, oi: bool) -> LinkStatus {
    LinkStatus { openit, oi }
  }

  #[test]
  fn status_from_a_temp_directory_is_none_one_or_both() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(status_in(dir.path()), status(false, false));
    fs::write(dir.path().join("openit"), []).unwrap();
    assert_eq!(status_in(dir.path()), status(true, false));
    fs::write(dir.path().join("oi"), []).unwrap();
    assert_eq!(status_in(dir.path()), status(true, true));
  }

  #[test]
  fn plan_maps_checkbox_state_to_create_and_remove_sets() {
    let none = status(false, false);
    let both = status(true, true);
    let openit_only = status(true, false);
    assert_eq!(initial_wanted(none), both);
    assert_eq!(initial_wanted(openit_only), openit_only);
    assert_eq!(
      plan(none, both),
      Plan {
        create: vec!["openit", "oi"],
        remove: vec![],
      }
    );
    assert_eq!(plan(both, openit_only), Plan { create: vec![], remove: vec!["oi"] });
    assert_eq!(
      plan(both, none),
      Plan {
        create: vec![],
        remove: vec!["openit", "oi"],
      }
    );
    assert_eq!(plan(both, both), Plan { create: vec![], remove: vec![] });
  }

  #[test]
  fn link_target_prefers_appimage_over_the_running_binary() {
    let exe = PathBuf::from("OpenIt");
    assert_eq!(
      resolve_link_target(&exe, Some("/tmp/OpenIt.AppImage")),
      PathBuf::from("/tmp/OpenIt.AppImage")
    );
    assert_eq!(resolve_link_target(&exe, Some("")), exe);
  }

  #[cfg(unix)]
  #[test]
  fn link_target_resolves_symlinks_when_appimage_is_unset() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("OpenIt");
    fs::write(&real, []).unwrap();
    let link = dir.path().join("current");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let resolved = fs::canonicalize(&real).unwrap();
    assert_eq!(resolve_link_target(&link, None), resolved);
  }

  #[test]
  fn nsis_preinstall_section_ships_cmd_shims_and_user_path() {
    let toml = include_str!("../Cargo.toml");
    let after = toml.split("[package.metadata.packager.nsis]").nth(1).unwrap();
    let section = after.split("[[package.metadata.packager.").next().unwrap();
    assert!(section.contains("preinstall-section"));
    assert!(section.contains("openit.cmd"));
    assert!(section.contains("oi.cmd"));
    assert!(section.contains("HKCU \"Environment\" \"Path\""));
    assert!(section.contains("IntFmt"));
    assert!(
      !section.contains(r"$\\n"),
      r"cargo-packager turns $\n into a real newline and breaks the NSI string"
    );
  }

  #[test]
  fn windows_shim_and_path_helpers() {
    let shim = windows_shim(std::path::Path::new(r"C:\Local\OpenIt.exe"));
    assert!(shim.contains("@echo off"));
    assert!(shim.contains(r#""C:\Local\OpenIt.exe" %*"#));
    assert_eq!(windows_path_adding("", r"C:\OpenIt\bin"), r"C:\OpenIt\bin");
    assert_eq!(
      windows_path_adding(r"C:\Windows", r"C:\OpenIt\bin"),
      r"C:\Windows;C:\OpenIt\bin"
    );
    assert_eq!(
      windows_path_adding(r"C:\OpenIt\bin;C:\Windows", r"C:\openit\bin"),
      r"C:\OpenIt\bin;C:\Windows"
    );
    assert_eq!(
      windows_path_removing(r"C:\Windows;C:\OpenIt\bin", r"C:\openit\bin"),
      r"C:\Windows"
    );
  }

  #[cfg(unix)]
  #[test]
  fn apply_creates_and_removes_links_in_a_temp_directory() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("OpenIt.bin");
    fs::write(&target, []).unwrap();
    apply_in(dir.path(), &target, &plan(status(false, false), status(true, true))).unwrap();
    assert_eq!(status_in(dir.path()), status(true, true));
    assert_eq!(
      fs::canonicalize(dir.path().join("openit")).unwrap(),
      fs::canonicalize(&target).unwrap()
    );
    apply_in(dir.path(), &target, &plan(status(true, true), status(true, false))).unwrap();
    assert_eq!(status_in(dir.path()), status(true, false));
  }
}
