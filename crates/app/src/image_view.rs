//! The image document surface: fit, zoom, pan, background, and metadata.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::drop::{apply_external_paths, external_paths_ring};
use gpui_kit::component::{ActiveTheme, Icon, TitleBar};
use gpui_kit::prelude::{FluentBuilder, InteractiveElement};
use gpui_kit::{
  App, AppContext, BorrowAppContext, Bounds, ContentMask, Context, Corners, DevicePixels, ExternalPaths, IntoElement,
  MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, PinchEvent, Pixels, Point, Render,
  ScrollDelta, ScrollWheelEvent, Size, Styled, Task, Window, canvas, div, px, size,
};
use openit_core::document::{ImageFormat, LoadedImage, Revision, load_image, probe_image};
use openit_core::kind::DocumentKind;
use openit_core::raster::{
  Decoded, ExportOptions, Transform, decode_still, encode_in_place, export, output_size, transformed,
};
use openit_core::recovery::{Draft, ImageDraft};
use openit_core::save::save_bytes;
use openit_core::session::SessionId;
use openit_core::watch::{FileWatch, Fingerprint};

use gpui_kit::component::input::{Redo, Undo};

use crate::actions::{
  ActualSize, CloseWindow, ColorTheme, CycleBackground, Export, FlipHorizontal, FlipVertical, GoToFile, RotateLeft,
  RotateRight, Save, SetImageBackground, ZoomIn, ZoomOut, ZoomToFit,
};
use crate::export_dialog::{ExportDialog, ExportEvent};
use crate::image_decode::DocumentImage;
use crate::nearby_picker::{NearbyPicker, NearbyPickerEvent};
use crate::session::{CHECKPOINT_DELAY, PendingCleanups, Recovery};
use crate::theme_picker::{ThemePicker, ThemePickerEvent};
use crate::title_bar::{file_name, toolbar_button};
use crate::{image_decode, svg};

/// Decode the document, apply the transform, and encode it for export.
fn encode_export(
  bytes: &[u8],
  format: ImageFormat,
  transform: Transform,
  base_dir: Option<&Path>,
  options: &ExportOptions,
) -> Result<Vec<u8>, String> {
  let decoded = if format == ImageFormat::Svg {
    let (width, height) = svg_export_size(bytes, base_dir, options)?;
    let (width, height, rgba) = svg::rasterize_to(bytes, (width, height), base_dir)?;
    let buffer =
      image::RgbaImage::from_raw(width, height, rgba).ok_or_else(|| "the rasterized image is malformed".to_owned())?;
    Decoded {
      image: transformed(&image::DynamicImage::ImageRgba8(buffer), transform),
      icc: None,
      has_alpha: true,
    }
  } else {
    let decoded = decode_still(bytes, format).map_err(|error| error.to_string())?;
    Decoded {
      image: transformed(&decoded.image, transform),
      icc: decoded.icc,
      has_alpha: decoded.has_alpha,
    }
  };
  export(&decoded, options).map_err(|error| error.to_string())
}

/// The pixel size an SVG export rasterizes at.
fn svg_export_size(bytes: &[u8], base_dir: Option<&Path>, options: &ExportOptions) -> Result<(u32, u32), String> {
  let intrinsic = svg::intrinsic_size(bytes, base_dir)?;
  Ok(output_size(intrinsic, options))
}

/// One checkpoint, ready to run off the UI thread.
type CheckpointWork = Box<dyn FnOnce() -> Result<(), String> + Send>;

/// Zoom bounds, so a wheel flick cannot leave the image invisible or fill memory.
const MIN_SCALE: f32 = 0.05;
const MAX_SCALE: f32 = 32.0;
/// One zoom step.
const ZOOM_STEP: f32 = 1.25;
/// How long a turn waits before it reaches the file, so a burst of turns is
/// one write. A draft covers the gap, and closing or quitting flushes it.
const SAVE_DELAY: std::time::Duration = std::time::Duration::from_secs(5);
/// How long the status bar keeps a save message.
const SAVED_NOTICE: std::time::Duration = std::time::Duration::from_secs(2);
/// Checkerboard cell size in window pixels.
const CHECKER_CELL: f32 = 8.0;

/// What fills the space around and behind the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ImageBackground {
  /// The window background from the active theme.
  Theme,
  /// Solid white.
  White,
  /// Solid black.
  Black,
  /// The transparency checkerboard.
  Checkerboard,
}

impl ImageBackground {
  /// The next background a click on the title bar icon selects.
  pub const fn next(self) -> Self {
    match self {
      Self::Checkerboard => Self::White,
      Self::White => Self::Black,
      Self::Theme | Self::Black => Self::Checkerboard,
    }
  }

  /// Label for the menu and the tooltip.
  pub const fn label(self) -> &'static str {
    match self {
      Self::Theme => "Theme",
      Self::White => "White",
      Self::Black => "Black",
      Self::Checkerboard => "Checkerboard",
    }
  }
}

/// How the image is scaled into the viewport.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Zoom {
  /// Scaled to fit, never past its own size.
  Fit,
  /// A fixed scale, where 1.0 is one image pixel per window pixel.
  Scale(f32),
}

/// One image document in its own window.
#[expect(
  clippy::struct_excessive_bools,
  reason = "the view tracks independent save, close, and presentation state"
)]
pub struct ImageView {
  path: PathBuf,
  format: ImageFormat,
  /// The bytes the file holds with no transform applied. Every encode starts
  /// here, so turns never stack re-encodings.
  source: std::sync::Arc<Vec<u8>>,
  width: u32,
  height: u32,
  file_size: u64,
  transform: Transform,
  /// A turn waiting for the debounce to reach the file.
  pending_write: bool,
  undo_stack: Vec<Transform>,
  redo_stack: Vec<Transform>,
  /// A short-lived message: what a save or a refused edit did.
  notice: Option<String>,
  notice_task: Option<Task<()>>,
  /// Clipboard pixels with no file yet; a save has to pick a destination.
  untitled: bool,
  session: SessionId,
  disk: Option<Fingerprint>,
  saving: bool,
  closing: bool,
  close_decided: bool,
  quitting: bool,
  last_error: Option<String>,
  checkpoint_task: Option<Task<()>>,
  save_task: Option<Task<()>>,
  prompt_task: Option<Task<()>>,
  reload_task: Option<Task<()>>,
  watch_task: Option<Task<()>>,
  #[allow(dead_code, reason = "the handle keeps the platform watcher alive")]
  watch: Option<FileWatch>,
  #[cfg(test)]
  watch_sender: Option<async_channel::Sender<()>>,
  window_handle: gpui_kit::AnyWindowHandle,
  decoded: Option<DocumentImage>,
  decode_error: Option<String>,
  decode_task: Option<Task<()>>,
  frame_index: usize,
  frame_task: Option<Task<()>>,
  zoom: Zoom,
  pan: Point<Pixels>,
  drag: Option<(Point<Pixels>, Point<Pixels>)>,
  background: ImageBackground,
  background_chosen: bool,
  viewport: Rc<Cell<Option<Bounds<Pixels>>>>,
  theme_picker: Option<gpui_kit::Entity<ThemePicker>>,
  nearby_picker: Option<gpui_kit::Entity<NearbyPicker>>,
  export_dialog: Option<gpui_kit::Entity<ExportDialog>>,
  focus: gpui_kit::FocusHandle,
}

impl ImageView {
  /// Open `loaded` in this window and start decoding it off the UI thread.
  /// Open a document. Pixels decoded ahead of the window let the first frame
  /// show the image; `None` decodes in the background instead.
  pub fn open(
    path: PathBuf,
    loaded: LoadedImage,
    decoded: Option<Result<DocumentImage, String>>,
    session: SessionId,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Self {
    let mut view = Self::new(
      path,
      loaded.format,
      loaded.bytes,
      (loaded.width, loaded.height),
      Some(loaded.disk),
      session,
      Transform::IDENTITY,
      window,
      cx,
    );
    match decoded {
      Some(Ok(decoded)) => {
        view.decode_task = None;
        view.adopt_decoded(decoded);
      },
      Some(Err(error)) => {
        view.decode_task = None;
        view.decode_error = Some(error);
      },
      None => {},
    }
    view.start_watch(window, cx);
    view
  }

  /// Take a decoded image, picking the default background the first time.
  fn adopt_decoded(&mut self, decoded: DocumentImage) {
    if !self.background_chosen && (decoded.has_alpha || self.format == ImageFormat::Svg) {
      self.background = ImageBackground::Checkerboard;
    }
    self.frame_index = 0;
    self.decode_error = None;
    self.decoded = Some(decoded);
  }

  /// Reopen a recovered image draft; the file on disk is not read.
  pub fn restore(draft: Draft, bytes: Vec<u8>, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let image = draft.image.unwrap_or(ImageDraft {
      format: ImageFormat::Png,
      transform: Transform::IDENTITY,
    });
    let kind = if image.format == ImageFormat::Svg {
      DocumentKind::Svg
    } else {
      DocumentKind::Image
    };
    let size = probe_image(&bytes, kind).map_or((0, 0), |(_, width, height)| (width, height));
    let mut view = Self::new(
      draft.path.unwrap_or_default(),
      image.format,
      bytes,
      size,
      draft.disk,
      draft.session,
      image.transform,
      window,
      cx,
    );
    if view.path.as_os_str().is_empty() {
      view.untitled = true;
    } else {
      view.start_watch(window, cx);
    }
    view
  }

  /// Open clipboard PNG bytes as an untitled document.
  pub fn from_clipboard(bytes: Vec<u8>, size: (u32, u32), window: &mut Window, cx: &mut Context<Self>) -> Self {
    let mut view = Self::new(
      PathBuf::new(),
      ImageFormat::Png,
      bytes,
      size,
      None,
      SessionId::new(),
      Transform::IDENTITY,
      window,
      cx,
    );
    // Clipboard pixels live only in this window until they are saved.
    view.untitled = true;
    view.schedule_checkpoint(cx);
    view
  }

  #[expect(clippy::too_many_arguments, reason = "one constructor for opening and restoring")]
  fn new(
    path: PathBuf,
    format: ImageFormat,
    bytes: Vec<u8>,
    size: (u32, u32),
    disk: Option<Fingerprint>,
    session: SessionId,
    transform: Transform,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Self {
    let file_size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    let mut view = Self {
      path,
      format,
      source: std::sync::Arc::new(bytes),
      width: size.0,
      height: size.1,
      file_size,
      transform,
      pending_write: false,
      undo_stack: Vec::new(),
      redo_stack: Vec::new(),
      notice: None,
      notice_task: None,
      untitled: false,
      session,
      disk,
      saving: false,
      closing: false,
      close_decided: false,
      quitting: false,
      last_error: None,
      checkpoint_task: None,
      save_task: None,
      prompt_task: None,
      reload_task: None,
      watch_task: None,
      watch: None,
      #[cfg(test)]
      watch_sender: None,
      window_handle: window.window_handle(),
      decoded: None,
      decode_error: None,
      decode_task: None,
      frame_index: 0,
      frame_task: None,
      zoom: Zoom::Fit,
      pan: Point::default(),
      drag: None,
      background: ImageBackground::Theme,
      background_chosen: false,
      viewport: Rc::default(),
      theme_picker: None,
      nearby_picker: None,
      export_dialog: None,
      focus: cx.focus_handle(),
    };
    window.focus(&view.focus, cx);
    Self::install_close_guard(window, cx);
    view.start_decode(cx);
    view
  }

  /// The document title: the file name, or `Untitled` before it has one.
  pub fn title(&self) -> String {
    self
      .path
      .file_name()
      .map_or_else(|| "Untitled".to_owned(), |name| name.to_string_lossy().into_owned())
  }
  pub(crate) fn path(&self) -> &Path {
    &self.path
  }

  /// Run the dirty-close prompt if needed, then `on_ready`. Cancel leaves the image.
  pub(crate) fn confirm_leave(
    &mut self,
    window: &mut Window,
    cx: &mut Context<Self>,
    on_ready: impl FnOnce(&mut Window, &mut App) + 'static,
  ) {
    if self.closing {
      return;
    }
    if !self.is_dirty() {
      on_ready(window, cx);
      return;
    }
    let answer = window.prompt(
      gpui_kit::PromptLevel::Warning,
      &format!("Save changes to {}?", self.title()),
      Some("Discard removes the recovered draft as well."),
      &["Save", "Discard", "Cancel"],
      cx,
    );
    self.prompt_task = Some(cx.spawn_in(window, async move |view, cx| match answer.await {
      Ok(0) => {
        let save = view.update_in(cx, |view, window, cx| {
          if view.pending_write {
            view.write(cx);
          } else {
            view.save(&Save, window, cx);
          }
          view.save_task.take()
        });
        if let Ok(Some(save)) = save {
          save.await;
        }
        let _ = view.update_in(cx, |view, window, cx| {
          if view.last_error.is_some() {
            return;
          }
          on_ready(window, cx);
        });
      },
      Ok(1) => {
        let _ = view.update_in(cx, |view, window, cx| {
          view.checkpoint_task = None;
          view.remove_draft(cx);
          on_ready(window, cx);
        });
      },
      _ => {},
    }));
  }

  pub(crate) fn end_session_without_draft(&mut self, cx: &Context<Self>) {
    self.checkpoint_task = None;
    self.remove_draft(cx);
  }

  /// The left half of the status bar.
  pub fn status_text(&self) -> String {
    let (width, height) = self.transform.apply_to_size(self.width, self.height);
    let mut text = format!("{width} x {height} · {} · {}", self.format.label(), format_size(self.file_size));
    if let Some(decoded) = &self.decoded {
      let frames = decoded.render.frame_count();
      if frames > 1 {
        use std::fmt::Write as _;
        let _ = write!(text, " · {frames} frames");
      }
    }
    text
  }

  /// Whether the window holds a change the file does not have.
  pub const fn is_dirty(&self) -> bool {
    self.untitled || self.pending_write
  }

  /// The size of the bytes this window holds.
  #[cfg(test)]
  pub(crate) fn byte_len(&self) -> usize {
    self.source.len()
  }

  /// Whether a rotation can be written back to this document.
  const fn can_edit_in_place(&self) -> bool {
    self.format.can_save_in_place()
  }

  /// The short-lived message in the status bar: a save, or a refused edit.
  pub fn notice(&self) -> Option<String> {
    self.notice.clone()
  }

  /// The failure to report in the status bar.
  pub fn last_error(&self) -> Option<&str> {
    self.last_error.as_deref()
  }

  /// Whether this view already settled its close decision, for the quit gate.
  pub const fn close_decided(&self) -> bool {
    self.close_decided
  }

  /// Image saves are immediate, so a close never waits on one.
  pub const fn close_after_save() -> bool {
    false
  }

  /// Mark the window as quitting; returns whether it was already closing.
  pub const fn begin_quit(&mut self) -> bool {
    let already_closing = self.closing;
    self.closing = true;
    self.quitting = true;
    already_closing
  }

  /// Reopen the window when the quit gate could not flush this draft.
  pub const fn abort_quit(&mut self) {
    self.closing = false;
    self.quitting = false;
    self.close_decided = false;
  }

  /// Write the draft now, so quitting cannot lose an unsaved rotation.
  pub fn flush_checkpoint(&mut self, cx: &Context<Self>) -> Task<Result<(), String>> {
    self.checkpoint_task = None;
    if let Some(write) = self.flush_pending_write(cx) {
      return write;
    }
    let Some(work) = self.checkpoint_work(cx) else {
      return Task::ready(Ok(()));
    };
    cx.background_spawn(async move { work() })
  }

  /// Images have no operation chain of their own; saves own their own task.
  pub fn chain_drained(&mut self, cx: &Context<Self>) -> Task<()> {
    let save = self.save_task.take();
    cx.spawn(async move |_, _| {
      if let Some(save) = save {
        save.await;
      }
    })
  }

  /// The zoom readout, once the image has been decoded and measured.
  pub fn zoom_percent(&self) -> Option<u32> {
    let scale = self.scale()?;
    Some(percent(scale))
  }

  /// What currently fills the space behind the image.
  pub const fn background(&self) -> ImageBackground {
    self.background
  }

  /// The decode failure to show in place of the image.
  pub fn decode_error(&self) -> Option<&str> {
    self.decode_error.as_deref()
  }

  /// Replace the background and remember that the choice was deliberate.
  pub fn set_background(&mut self, background: ImageBackground, cx: &mut Context<Self>) {
    self.background = background;
    self.background_chosen = true;
    cx.notify();
  }

  fn base_dir(&self) -> Option<PathBuf> {
    self
      .path
      .parent()
      .filter(|dir| !dir.as_os_str().is_empty())
      .map(Path::to_path_buf)
  }

  /// Decode (or re-decode after a transform) on a background thread.
  fn start_decode(&mut self, cx: &Context<Self>) {
    let handle = self.window_handle;
    let bytes = std::sync::Arc::clone(&self.source);
    let format = self.format;
    let transform = self.transform;
    let base_dir = self.base_dir();
    self.decode_task = Some(cx.spawn(async move |view, cx| {
      let result = cx
        .background_spawn(async move {
          if format == ImageFormat::Svg {
            svg::decode_document(&bytes, transform, base_dir.as_deref())
          } else {
            image_decode::decode_document(&bytes, format, transform)
          }
        })
        .await;
      let _ = view.update(cx, |view, cx| {
        match result {
          Ok(decoded) => {
            if !view.background_chosen && (decoded.has_alpha || view.format == ImageFormat::Svg) {
              view.background = ImageBackground::Checkerboard;
            }
            view.frame_index = 0;
            view.decode_error = None;
            view.decoded = Some(decoded);
            view.start_animation(cx);
          },
          Err(error) => {
            tracing::error!(%error, path = %view.path.display(), "image decode failed");
            view.decoded = None;
            view.decode_error = Some(error);
          },
        }
        cx.notify();
      });
      refresh(handle, cx);
    }));
  }

  /// Cycle animation frames at the delays the file declares.
  fn start_animation(&mut self, cx: &Context<Self>) {
    self.frame_task = None;
    let handle = self.window_handle;
    let Some(decoded) = &self.decoded else { return };
    let count = decoded.render.frame_count();
    if count < 2 {
      return;
    }
    self.frame_task = Some(cx.spawn(async move |view, cx| {
      loop {
        let delay = view
          .read_with(cx, |view, _| {
            view.decoded.as_ref().map(|decoded| {
              let (numerator, denominator) = decoded.render.delay(view.frame_index).numer_denom_ms();
              std::time::Duration::from_millis(u64::from(numerator.max(20)) / u64::from(denominator.max(1)))
            })
          })
          .ok()
          .flatten();
        let Some(delay) = delay else { return };
        cx.background_executor().timer(delay).await;
        if view
          .update(cx, |view, cx| {
            view.frame_index = (view.frame_index + 1) % count;
            cx.notify();
          })
          .is_err()
        {
          return;
        }
        refresh(handle, cx);
      }
    }));
  }

  /// Apply a rotation or flip. A document with a path keeps the file in step,
  /// the way Preview does; the change is one Cmd+Z away from coming back.
  fn apply_transform(&mut self, next: Transform, cx: &mut Context<Self>) {
    if self.closing || next == self.transform {
      return;
    }
    self.undo_stack.push(self.transform);
    self.redo_stack.clear();
    self.settle_transform(next, cx);
  }

  /// Move to `next`, redraw, and persist it however this document can.
  fn settle_transform(&mut self, next: Transform, cx: &mut Context<Self>) {
    self.transform = next;
    self.last_error = None;
    self.start_decode(cx);
    if self.untitled {
      self.schedule_checkpoint(cx);
    } else if self.can_edit_in_place() {
      self.pending_write = true;
      // The draft covers the window between the turn and the write.
      self.schedule_checkpoint(cx);
      self.schedule_write(cx);
    } else {
      self.announce(format!("{} cannot be saved; use File > Export", self.format.label()), cx);
    }
    cx.notify();
  }

  /// Step back through the rotations and flips applied in this window.
  fn undo(&mut self, _: &Undo, _window: &mut Window, cx: &mut Context<Self>) {
    let Some(previous) = self.undo_stack.pop() else {
      return;
    };
    self.redo_stack.push(self.transform);
    self.settle_transform(previous, cx);
  }

  /// Step forward again after an undo.
  fn redo(&mut self, _: &Redo, _window: &mut Window, cx: &mut Context<Self>) {
    let Some(next) = self.redo_stack.pop() else {
      return;
    };
    self.undo_stack.push(self.transform);
    self.settle_transform(next, cx);
  }

  fn rotate_left(&mut self, _: &RotateLeft, _window: &mut Window, cx: &mut Context<Self>) {
    self.apply_transform(self.transform.rotate_ccw(), cx);
  }

  fn rotate_right(&mut self, _: &RotateRight, _window: &mut Window, cx: &mut Context<Self>) {
    self.apply_transform(self.transform.rotate_cw(), cx);
  }

  fn flip_horizontal(&mut self, _: &FlipHorizontal, _window: &mut Window, cx: &mut Context<Self>) {
    self.apply_transform(self.transform.flip_horizontal(), cx);
  }

  fn flip_vertical(&mut self, _: &FlipVertical, _window: &mut Window, cx: &mut Context<Self>) {
    self.apply_transform(self.transform.flip_vertical(), cx);
  }

  /// Cmd+S on a clipboard image picks a destination. A document with a path is
  /// already saved, so the shortcut has nothing left to do.
  fn save(&mut self, _: &Save, window: &mut Window, cx: &mut Context<Self>) {
    if self.saving || self.closing {
      return;
    }
    if self.untitled {
      let saved = Self::save_as(window, cx);
      self.prompt_task = Some(cx.spawn(async move |_, _| {
        saved.await;
      }));
      return;
    }
    if !self.can_edit_in_place() {
      self.announce(format!("{} cannot be saved; use File > Export", self.format.label()), cx);
      cx.notify();
    }
  }

  /// Write a turn that is still waiting on its debounce, now. Returns `None`
  /// when the file is already up to date.
  pub fn flush_pending_write(&mut self, cx: &Context<Self>) -> Option<Task<Result<(), String>>> {
    if !self.pending_write {
      return None;
    }
    self.save_task = None;
    self.pending_write = false;
    let source = std::sync::Arc::clone(&self.source);
    let (format, transform, path) = (self.format, self.transform, self.path.clone());
    Some(cx.background_spawn(async move {
      let encoded = if transform.is_identity() {
        source.as_ref().clone()
      } else {
        encode_in_place(&source, format, transform).map_err(|error| error.to_string())?
      };
      save_bytes(&path, Revision::INITIAL.next(), &encoded)
        .map(|_| ())
        .map_err(|error| error.to_string())
    }))
  }

  /// Let a burst of turns settle, then write once.
  fn schedule_write(&mut self, cx: &Context<Self>) {
    self.save_task = Some(cx.spawn(async move |view, cx| {
      cx.background_executor().timer(SAVE_DELAY).await;
      let _ = view.update(cx, |view, cx| {
        if view.pending_write {
          view.write(cx);
        }
      });
    }));
  }

  /// Write the source through the current transform. Encoding always starts
  /// from the bytes the file had, so repeated turns never stack losses and a
  /// full turn back restores the original file byte for byte.
  fn write(&mut self, cx: &mut Context<Self>) {
    self.saving = true;
    self.last_error = None;
    cx.notify();
    let source = std::sync::Arc::clone(&self.source);
    let format = self.format;
    let transform = self.transform;
    let path = self.path.clone();
    let revision = Revision::INITIAL.next();
    self.save_task = Some(cx.spawn(async move |view, cx| {
      let result = cx
        .background_spawn(async move {
          let encoded = if transform.is_identity() {
            source.as_ref().clone()
          } else {
            encode_in_place(&source, format, transform)?
          };
          let saved = save_bytes(&path, revision, &encoded)?;
          Ok::<_, openit_core::Error>((encoded.len(), saved))
        })
        .await;
      let _ = view.update(cx, |view, cx| {
        view.saving = false;
        match result {
          Ok((len, saved)) => {
            view.file_size = u64::try_from(len).unwrap_or(u64::MAX);
            view.disk = Some(saved.fingerprint);
            view.pending_write = false;
            view.remove_draft(cx);
            view.announce("Saved".to_owned(), cx);
          },
          Err(error) => {
            tracing::error!(%error, "image save failed");
            view.last_error = Some(error.to_string());
          },
        }
        cx.notify();
      });
    }));
  }

  /// Show a message in the status bar, then let it fade.
  fn announce(&mut self, message: String, cx: &Context<Self>) {
    self.notice = Some(message);
    let handle = self.window_handle;
    self.notice_task = Some(cx.spawn(async move |view, cx| {
      cx.background_executor().timer(SAVED_NOTICE).await;
      let _ = view.update(cx, |view, cx| {
        view.notice = None;
        cx.notify();
      });
      refresh(handle, cx);
    }));
  }

  /// Ask for a destination, then write the pixels there. Resolves to whether the
  /// file was written.
  #[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "GPUI takes the window and context mutably to spawn the prompt"
  )]
  fn save_as(window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
    let directory = dirs::picture_dir()
      .or_else(dirs::home_dir)
      .unwrap_or_else(|| PathBuf::from("."));
    let prompt = cx.prompt_for_new_path(&directory, Some("Untitled.png"));
    cx.spawn_in(window, async move |view, cx| {
      let chosen = match prompt.await {
        Ok(Ok(Some(path))) => path,
        Ok(Ok(None)) => return false,
        Ok(Err(error)) => {
          let _ = view.update(cx, |view, cx| {
            view.last_error = Some(format!("Save As failed: {error}"));
            cx.notify();
          });
          return false;
        },
        Err(error) => {
          tracing::debug!(%error, "the save prompt closed without an answer");
          return false;
        },
      };
      let write = view.update_in(cx, |view, window, cx| {
        view.path = chosen;
        view.untitled = false;
        view.write(cx);
        view.start_watch(window, cx);
        view.save_task.take()
      });
      let Ok(write) = write else {
        return false;
      };
      if let Some(write) = write {
        write.await;
      }
      view.read_with(cx, |view, _| view.last_error.is_none()).unwrap_or(false)
    })
  }

  /// Open the export dialog for the image as it is shown.
  fn open_export(&mut self, _: &Export, window: &mut Window, cx: &mut Context<Self>) {
    let (width, height) = self.transform.apply_to_size(self.width, self.height);
    let has_alpha = self.decoded.as_ref().is_some_and(|decoded| decoded.has_alpha);
    let stem = self
      .path
      .file_stem()
      .map_or_else(|| "Untitled".to_owned(), |stem| stem.to_string_lossy().into_owned());
    let dialog = cx.new(|cx| ExportDialog::new((width, height), has_alpha, &stem, window, cx));
    cx.subscribe_in(&dialog, window, |view, dialog, event: &ExportEvent, window, cx| {
      match event {
        ExportEvent::Confirm(options) => {
          let suggested = dialog.read(cx).suggested_name();
          view.export_dialog = None;
          view.run_export(options.clone(), suggested.as_str(), window, cx);
        },
        ExportEvent::Close => view.export_dialog = None,
      }
      cx.notify();
    })
    .detach();
    self.export_dialog = Some(dialog);
    cx.notify();
  }

  /// Ask where to write, then encode off the UI thread.
  #[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "GPUI takes the window and context mutably to spawn the prompt"
  )]
  fn run_export(&mut self, options: ExportOptions, suggested: &str, window: &mut Window, cx: &mut Context<Self>) {
    let directory = self
      .path
      .parent()
      .filter(|parent| !parent.as_os_str().is_empty())
      .map(Path::to_path_buf)
      .or_else(dirs::picture_dir)
      .or_else(dirs::home_dir)
      .unwrap_or_else(|| PathBuf::from("."));
    let prompt = cx.prompt_for_new_path(&directory, Some(suggested));
    let bytes = std::sync::Arc::clone(&self.source);
    let format = self.format;
    let transform = self.transform;
    let base_dir = self.base_dir();
    self.prompt_task = Some(cx.spawn_in(window, async move |view, cx| {
      let Ok(Ok(Some(target))) = prompt.await else {
        return;
      };
      let result = cx
        .background_spawn(async move { encode_export(&bytes, format, transform, base_dir.as_deref(), &options) })
        .await
        .and_then(|encoded| {
          save_bytes(&target, Revision::INITIAL.next(), &encoded)
            .map(|_| ())
            .map_err(|error| error.to_string())
        });
      let _ = view.update(cx, |view, cx| {
        view.last_error = match result {
          Ok(()) => None,
          Err(error) => {
            tracing::error!(%error, "export failed");
            Some(format!("Export failed: {error}"))
          },
        };
        cx.notify();
      });
    }));
  }

  /// The work one checkpoint performs, or `None` when there is nothing to keep.
  fn checkpoint_work(&self, cx: &Context<Self>) -> Option<CheckpointWork> {
    if !self.is_dirty() {
      return None;
    }
    let store = cx.try_global::<Recovery>().and_then(|recovery| recovery.0.clone())?;
    let draft = Draft {
      session: self.session,
      path: (!self.path.as_os_str().is_empty()).then(|| self.path.clone()),
      disk: self.disk,
      text: String::new(),
      cursor: 0,
      image: Some(ImageDraft {
        format: self.format,
        transform: self.transform,
      }),
      schema: None,
    };
    let bytes = std::sync::Arc::clone(&self.source);
    Some(Box::new(move || {
      store.write_blob(draft.session, &bytes).map_err(|error| error.to_string())?;
      store.checkpoint(&draft).map_err(|error| error.to_string())
    }))
  }

  /// Write a draft after the usual pause.
  fn schedule_checkpoint(&mut self, cx: &Context<Self>) {
    if self.closing {
      return;
    }
    self.checkpoint_task = Some(cx.spawn(async move |view, cx| {
      cx.background_executor().timer(CHECKPOINT_DELAY).await;
      let work = view.update(cx, |view, cx| view.checkpoint_work(cx)).ok().flatten();
      let Some(work) = work else { return };
      let result = cx.background_spawn(async move { work() }).await;
      if let Err(error) = result {
        let _ = view.update(cx, |view, cx| {
          view.last_error = Some(format!("Draft could not be written: {error}"));
          cx.notify();
        });
      }
    }));
  }

  /// Drop this document's draft and its pixels.
  fn remove_draft(&self, cx: &Context<Self>) {
    let Some(store) = cx.try_global::<Recovery>().and_then(|recovery| recovery.0.clone()) else {
      return;
    };
    let session = self.session;
    cx.background_spawn(async move {
      if let Err(error) = store.remove(session) {
        tracing::error!(%error, "could not remove the image draft");
      }
    })
    .detach();
  }

  /// Ask before dropping an unsaved rotation, then close.
  fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.request_close_decision(window, cx).detach();
  }

  /// Whether the window holds an image the user has not yet decided to save or drop.
  pub const fn needs_close_decision(&self) -> bool {
    self.is_dirty() && !self.closing
  }

  /// Close, asking first when the image has nowhere to go. Resolves to whether the
  /// window is on its way out: `false` when the user cancels the prompt or Save As.
  pub(crate) fn request_close_decision(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
    if self.closing {
      return Task::ready(true);
    }
    if self.pending_write {
      // The turn is already the user's decision; the close just hurries it.
      self.write(cx);
      let save = self.save_task.take();
      self.close_decided = true;
      self.save_task = Some(cx.spawn(async move |view, cx| {
        if let Some(save) = save {
          save.await;
        }
        let _ = view.update(cx, Self::finish_close);
      }));
      return Task::ready(true);
    }
    if !self.is_dirty() {
      self.finish_close(cx);
      return Task::ready(true);
    }
    let answer = window.prompt(
      gpui_kit::PromptLevel::Warning,
      &format!("Save changes to {}?", self.title()),
      Some("Discard removes the recovered draft as well."),
      &["Save", "Discard", "Cancel"],
      cx,
    );
    let (decided, decision) = async_channel::bounded(1);
    self.prompt_task = Some(cx.spawn_in(window, async move |view, cx| {
      let proceed = match answer.await {
        Ok(0) => {
          let saved = match view.update_in(cx, |_, window, cx| Self::save_as(window, cx)) {
            Ok(saved) => saved.await,
            Err(_) => false,
          };
          saved
            && view
              .update(cx, |view, cx| {
                view.close_decided = true;
                view.finish_close(cx);
              })
              .is_ok()
        },
        Ok(1) => view
          .update(cx, |view, cx| {
            view.close_decided = true;
            view.remove_draft(cx);
            view.finish_close(cx);
          })
          .is_ok(),
        _ => {
          let _ = view.update(cx, |view, _| view.close_decided = false);
          false
        },
      };
      let _ = decided.try_send(proceed);
    }));
    cx.spawn(async move |_, _| decision.recv().await.unwrap_or(false))
  }

  /// Close the window once the draft decision is settled.
  fn finish_close(&mut self, cx: &mut Context<Self>) {
    self.closing = true;
    self.close_decided = true;
    self.checkpoint_task = None;
    self.reload_task = None;
    let handle = self.window_handle;
    let cleanup = cx.spawn(async move |_, cx| {
      let _ = cx.update_window(handle, |_, window, _| window.remove_window());
    });
    cx.update_default_global::<PendingCleanups, _>(|pending, _| pending.0.push(cleanup));
  }

  fn close(&mut self, _: &CloseWindow, window: &mut Window, cx: &mut Context<Self>) {
    self.request_close(window, cx);
  }

  /// A window close from the platform runs the same prompt.
  fn install_close_guard(window: &Window, cx: &Context<Self>) {
    let entity = cx.entity();
    window.on_window_should_close(cx, move |window, cx| {
      entity.update(cx, |view, cx| {
        let quitting = cx.try_global::<crate::QuitCommitted>().is_some_and(|quit| quit.0);
        if view.closing && (quitting || view.close_decided) {
          true
        } else {
          view.request_close(window, cx);
          false
        }
      })
    });
  }

  /// Watch the source file so a clean document follows edits made elsewhere.
  fn start_watch(&mut self, window: &Window, cx: &Context<Self>) {
    let (sender, receiver) = async_channel::bounded::<()>(1);
    #[cfg(test)]
    {
      self.watch_sender = Some(sender);
    }
    #[cfg(not(test))]
    {
      match FileWatch::new(&self.path, move || {
        let _ = sender.try_send(());
      }) {
        Ok(watch) => self.watch = Some(watch),
        Err(error) => {
          tracing::warn!(%error, "file watch unavailable");
          return;
        },
      }
    }
    self.watch_task = Some(cx.spawn_in(window, async move |view, cx| {
      while cx
        .background_spawn({
          let receiver = receiver.clone();
          async move { receiver.recv().await }
        })
        .await
        .is_ok()
      {
        cx.background_executor().timer(std::time::Duration::from_millis(150)).await;
        while receiver.try_recv().is_ok() {}
        if view.update(cx, Self::on_disk_change).is_err() {
          break;
        }
      }
    }));
  }

  /// Reload a clean document after the file changed underneath it.
  fn on_disk_change(&mut self, cx: &mut Context<Self>) {
    if self.closing || self.is_dirty() || self.path.as_os_str().is_empty() {
      return;
    }
    let path = self.path.clone();
    self.reload_task = Some(cx.spawn(async move |view, cx| {
      let result = cx.background_spawn(async move { load_image(&path) }).await;
      let _ = view.update(cx, |view, cx| {
        match result {
          Ok(loaded) => {
            if view.closing || view.is_dirty() {
              return;
            }
            view.file_size = u64::try_from(loaded.bytes.len()).unwrap_or(u64::MAX);
            view.source = std::sync::Arc::new(loaded.bytes);
            view.format = loaded.format;
            view.width = loaded.width;
            view.height = loaded.height;
            view.disk = Some(loaded.disk);
            view.transform = Transform::IDENTITY;
            view.undo_stack.clear();
            view.redo_stack.clear();
            view.start_decode(cx);
          },
          Err(error) => tracing::warn!(%error, "reload after an external change failed"),
        }
        cx.notify();
      });
    }));
  }

  /// Drive the watch pump from a test without a platform watcher.
  #[cfg(test)]
  pub(crate) fn notify_disk_change_for_test(&mut self, cx: &mut Context<Self>) {
    Self::on_disk_change(self, cx);
  }

  /// Pixel size of the decoded texture, in image pixels.
  fn texture_size(&self) -> Option<Size<Pixels>> {
    let decoded = self.decoded.as_ref()?;
    let size = decoded
      .render
      .size(self.frame_index.min(decoded.render.frame_count().saturating_sub(1)));
    Some(size_in_pixels(size))
  }

  /// The scale in effect, resolving `Fit` against the current viewport.
  fn scale(&self) -> Option<f32> {
    let texture = self.texture_size()?;
    match self.zoom {
      Zoom::Scale(scale) => Some(scale),
      Zoom::Fit => {
        let viewport = self.viewport.get()?;
        let fit = (viewport.size.width / texture.width).min(viewport.size.height / texture.height);
        Some(fit.clamp(MIN_SCALE, 1.0))
      },
    }
  }

  /// Where the image is painted inside `viewport`.
  fn image_bounds(&self, viewport: Bounds<Pixels>) -> Option<Bounds<Pixels>> {
    let drawn = self.drawn_size()?;
    Some(placed(viewport, drawn, clamp_pan(self.pan, viewport.size, drawn)))
  }

  /// The size the image occupies on screen at the scale in effect.
  fn drawn_size(&self) -> Option<Size<Pixels>> {
    let texture = self.texture_size()?;
    let scale = self.scale()?;
    Some(size(texture.width * scale, texture.height * scale))
  }

  /// The pan in effect, never past the edges of the image.
  #[cfg(test)]
  pub fn pan(&self) -> Point<Pixels> {
    match (self.viewport.get(), self.drawn_size()) {
      (Some(viewport), Some(drawn)) => clamp_pan(self.pan, viewport.size, drawn),
      _ => self.pan,
    }
  }

  /// Move the image by `delta`, stopping at its edges.
  pub fn pan_by(&mut self, delta: Point<Pixels>, cx: &mut Context<Self>) {
    self.set_pan(
      Point {
        x: self.pan.x + delta.x,
        y: self.pan.y + delta.y,
      },
      cx,
    );
  }

  /// Move the image to `pan`, stopping at its edges.
  fn set_pan(&mut self, pan: Point<Pixels>, cx: &mut Context<Self>) {
    self.pan = match (self.viewport.get(), self.drawn_size()) {
      (Some(viewport), Some(drawn)) => clamp_pan(pan, viewport.size, drawn),
      _ => pan,
    };
    cx.notify();
  }

  fn set_scale(&mut self, scale: f32, anchor: Option<Point<Pixels>>, cx: &mut Context<Self>) {
    let Some(previous) = self.scale() else { return };
    let scale = scale.clamp(MIN_SCALE, MAX_SCALE);
    if let (Some(anchor), Some(viewport)) = (anchor, self.viewport.get())
      && let Some(bounds) = self.image_bounds(viewport)
    {
      // Keep the point under the cursor where it is.
      let ratio = scale / previous;
      self.pan.x += (bounds.origin.x - anchor.x) * (ratio - 1.);
      self.pan.y += (bounds.origin.y - anchor.y) * (ratio - 1.);
    }
    self.zoom = Zoom::Scale(scale);
    // The new scale changes how far the image may travel.
    self.set_pan(self.pan, cx);
  }

  fn zoom_in(&mut self, _: &ZoomIn, _window: &mut Window, cx: &mut Context<Self>) {
    if let Some(scale) = self.scale() {
      self.set_scale(scale * ZOOM_STEP, None, cx);
    }
  }

  fn zoom_out(&mut self, _: &ZoomOut, _window: &mut Window, cx: &mut Context<Self>) {
    if let Some(scale) = self.scale() {
      self.set_scale(scale / ZOOM_STEP, None, cx);
    }
  }

  fn zoom_to_fit(&mut self, _: &ZoomToFit, _window: &mut Window, cx: &mut Context<Self>) {
    self.zoom = Zoom::Fit;
    self.pan = Point::default();
    cx.notify();
  }

  fn actual_size(&mut self, _: &ActualSize, _window: &mut Window, cx: &mut Context<Self>) {
    self.pan = Point::default();
    self.set_scale(1.0, None, cx);
  }

  fn cycle_background(&mut self, _: &CycleBackground, _window: &mut Window, cx: &mut Context<Self>) {
    self.set_background(self.background.next(), cx);
  }

  #[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "GPUI action handlers take the action by reference"
  )]
  fn choose_background(&mut self, action: &SetImageBackground, _window: &mut Window, cx: &mut Context<Self>) {
    let background = action.background;
    self.set_background(background, cx);
  }

  fn open_theme_picker(&mut self, _: &ColorTheme, window: &mut Window, cx: &mut Context<Self>) {
    let picker = cx.new(|cx| ThemePicker::new(window, cx));
    cx.subscribe_in(&picker, window, |view, _, event: &ThemePickerEvent, _window, cx| {
      if matches!(event, ThemePickerEvent::Close) {
        view.theme_picker = None;
        cx.notify();
      }
    })
    .detach();
    self.theme_picker = Some(picker);
    cx.notify();
  }

  fn open_nearby_picker(&mut self, _: &GoToFile, window: &mut Window, cx: &mut Context<Self>) {
    let path = (!self.path.as_os_str().is_empty()).then_some(self.path.as_path());
    let picker = cx.new(|cx| NearbyPicker::new(path, window, cx));
    cx.subscribe_in(&picker, window, |view, _, event: &NearbyPickerEvent, _window, cx| {
      if matches!(event, NearbyPickerEvent::Close) {
        view.nearby_picker = None;
        cx.notify();
      }
    })
    .detach();
    self.nearby_picker = Some(picker);
    cx.notify();
  }

  /// The two greys of the transparency checkerboard, derived from the theme.
  fn checker_colors(cx: &App) -> (gpui_kit::Hsla, gpui_kit::Hsla) {
    let base = cx.theme().background;
    let light = gpui_kit::hsla(base.h, base.s * 0.2, (base.l + 0.06).clamp(0., 1.), 1.);
    let dark = gpui_kit::hsla(base.h, base.s * 0.2, (base.l - 0.06).clamp(0., 1.), 1.);
    (light, dark)
  }

  fn render_title_row(&self, cx: &Context<Self>) -> impl IntoElement {
    let theme = cx.theme();
    let title = file_name(
      self.title(),
      self.is_dirty(),
      cx,
      cx.listener(|view, _, window, cx| view.open_nearby_picker(&GoToFile, window, cx)),
    );
    let mut actions = div()
      .flex()
      .items_center()
      .gap_2()
      // Clicks on these belong to the buttons: without this the title bar sees
      // them and macOS zooms the window on the second one.
      .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
      .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation());
    if self.can_edit_in_place() || self.untitled {
      actions = actions.child(toolbar_button(
        "rotate-left",
        Icon::empty().path("icons/rotate-ccw-square.svg"),
        "Rotate Left (Cmd+L)",
        cx,
        cx.listener(|view, _, window, cx| view.rotate_left(&RotateLeft, window, cx)),
      ));
    }
    // The theme picker lives in View > Color Theme... and Cmd+K Cmd+T.
    let actions = actions.child(toolbar_button(
      "image-background",
      Icon::empty().path("icons/swatch-book.svg"),
      format!(
        "Background: {} (click for {})",
        self.background().label(),
        self.background().next().label()
      ),
      cx,
      cx.listener(|view, _, window, cx| view.cycle_background(&CycleBackground, window, cx)),
    ));
    TitleBar::new().border_0().bg(theme.background).child(
      div()
        .flex()
        .items_center()
        .gap_3()
        .w_full()
        .h_full()
        .pr_2()
        .child(title)
        .child(actions),
    )
  }

  fn render_status_bar(&self, cx: &Context<Self>) -> impl IntoElement {
    let theme = cx.theme();
    div()
      // Its own row under the image: an overlay bar cuts into a picture that
      // has no padding of its own.
      .flex_shrink_0()
      .flex()
      .items_center()
      .gap_4()
      .h_6()
      .px_3()
      .bg(theme.background)
      .text_xs()
      .text_color(theme.muted_foreground)
      .child(self.status_text())
      .when_some(self.notice(), gpui_kit::ParentElement::child)
      .when_some(self.decode_error().map(str::to_owned), |bar, error| {
        bar.child(div().text_color(theme.danger).child(error))
      })
      .when_some(self.last_error().map(str::to_owned), |bar, error| {
        bar.child(div().text_color(theme.danger).child(error))
      })
      .child(div().flex_1())
      .children(self.zoom_percent().map(|percent| div().child(format!("{percent}%"))))
  }

  fn render_body(&self, cx: &Context<Self>) -> gpui_kit::AnyElement {
    if let Some(error) = self.decode_error().map(str::to_owned) {
      let theme = cx.theme();
      return div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .p_8()
        .text_sm()
        .text_color(theme.muted_foreground)
        .child(format!("{} could not be shown: {error}", self.title()))
        .into_any_element();
    }
    let background = self.background;
    let theme_background = cx.theme().background;
    let checker = Self::checker_colors(cx);
    // The canvas records the viewport it was given, so `Fit` can resolve on the
    // next frame without touching the view during layout.
    let viewport = Rc::clone(&self.viewport);
    let plan = PlanSource {
      decoded: self.decoded.clone(),
      zoom: self.zoom,
      pan: self.pan,
      frame: self.frame_index,
    };
    div()
      .id("image-surface")
      .relative()
      .flex_1()
      .min_h_0()
      .overflow_hidden()
      .on_scroll_wheel(cx.listener(Self::on_scroll))
      .on_pinch(cx.listener(Self::on_pinch))
      .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
      .on_mouse_move(cx.listener(Self::on_mouse_move))
      .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
      .child(
        canvas(
          move |bounds, _window, _cx| {
            let changed = viewport.replace(Some(bounds)) != Some(bounds);
            (plan.paint_plan(bounds), changed)
          },
          move |bounds, (plan, viewport_changed): (Option<PaintPlan>, bool), window, _cx| {
            if viewport_changed {
              window.refresh();
            }
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
              paint_background(window, bounds, background, theme_background, checker);
              if let Some(plan) = plan
                && let Err(error) =
                  window.paint_image(bounds, plan.image, Corners::default(), plan.render, plan.frame, false)
              {
                tracing::error!(%error, "painting the image failed");
              }
            });
          },
        )
        .absolute()
        .inset_0(),
      )
      .into_any_element()
  }

  fn on_scroll(&mut self, event: &ScrollWheelEvent, _window: &mut Window, cx: &mut Context<Self>) {
    let delta = match event.delta {
      ScrollDelta::Pixels(point) => point,
      ScrollDelta::Lines(point) => Point {
        x: px(point.x * 20.),
        y: px(point.y * 20.),
      },
    };
    if event.modifiers.secondary() {
      if let Some(scale) = self.scale() {
        let step = 1. + f32::from(delta.y) / 200.;
        self.set_scale(scale * step.clamp(0.5, 2.0), Some(event.position), cx);
      }
      return;
    }
    self.pan_by(delta, cx);
  }

  fn on_pinch(&mut self, event: &PinchEvent, _window: &mut Window, cx: &mut Context<Self>) {
    if let Some(scale) = self.scale() {
      self.set_scale(scale * (1. + event.delta), Some(event.position), cx);
    }
  }

  fn on_mouse_down(&mut self, event: &MouseDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
    self.drag = Some((event.position, self.pan));
    cx.notify();
  }

  fn on_mouse_move(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
    let Some((anchor, start)) = self.drag else {
      return;
    };
    if !event.dragging() {
      self.drag = None;
      return;
    }
    self.set_pan(
      Point {
        x: start.x + (event.position.x - anchor.x),
        y: start.y + (event.position.y - anchor.y),
      },
      cx,
    );
  }

  fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
    self.drag = None;
    cx.notify();
  }
}

/// Centre `drawn` in `viewport`, offset by `pan`.
fn placed(viewport: Bounds<Pixels>, drawn: Size<Pixels>, pan: Point<Pixels>) -> Bounds<Pixels> {
  Bounds {
    origin: Point {
      x: viewport.origin.x + (viewport.size.width - drawn.width) / 2. + pan.x,
      y: viewport.origin.y + (viewport.size.height - drawn.height) / 2. + pan.y,
    },
    size: drawn,
  }
}

/// Keep the image against the viewport: an image that fits stays centred, and
/// a larger one stops when its edge reaches the edge of the window.
fn clamp_pan(pan: Point<Pixels>, viewport: Size<Pixels>, drawn: Size<Pixels>) -> Point<Pixels> {
  let limit = |drawn: Pixels, viewport: Pixels| (drawn - viewport).max(px(0.)) / 2.;
  let (horizontal, vertical) = (limit(drawn.width, viewport.width), limit(drawn.height, viewport.height));
  Point {
    x: pan.x.clamp(-horizontal, horizontal),
    y: pan.y.clamp(-vertical, vertical),
  }
}

/// Ask a window for a frame. Work that finishes off the UI thread has to say
/// so: marking the view dirty alone does not schedule one.
fn refresh(handle: gpui_kit::AnyWindowHandle, cx: &mut gpui_kit::AsyncApp) {
  if let Err(error) = cx.update_window(handle, |_, window, _| window.refresh()) {
    tracing::debug!(%error, "the window closed before it could be redrawn");
  }
}

/// The view state one painted frame reads, captured before layout.
struct PlanSource {
  decoded: Option<DocumentImage>,
  zoom: Zoom,
  pan: Point<Pixels>,
  frame: usize,
}

impl PlanSource {
  /// Where the image lands inside `viewport`, at the scale in effect.
  fn paint_plan(&self, viewport: Bounds<Pixels>) -> Option<PaintPlan> {
    let decoded = self.decoded.as_ref()?;
    let frame = self.frame.min(decoded.render.frame_count().saturating_sub(1));
    let texture = size_in_pixels(decoded.render.size(frame));
    let scale = match self.zoom {
      Zoom::Scale(scale) => scale,
      Zoom::Fit => (viewport.size.width / texture.width)
        .min(viewport.size.height / texture.height)
        .clamp(MIN_SCALE, 1.0),
    };
    let drawn = size(texture.width * scale, texture.height * scale);
    Some(PaintPlan {
      image: placed(viewport, drawn, clamp_pan(self.pan, viewport.size, drawn)),
      render: std::sync::Arc::clone(&decoded.render),
      frame,
    })
  }
}

/// The image placement one painted frame needs.
struct PaintPlan {
  image: Bounds<Pixels>,
  render: std::sync::Arc<gpui_kit::RenderImage>,
  frame: usize,
}

/// Fill the viewport behind the image.
fn paint_background(
  window: &mut Window,
  bounds: Bounds<Pixels>,
  background: ImageBackground,
  theme_background: gpui_kit::Hsla,
  checker: (gpui_kit::Hsla, gpui_kit::Hsla),
) {
  let solid = match background {
    ImageBackground::Theme => Some(theme_background),
    ImageBackground::White => Some(gpui_kit::hsla(0., 0., 1., 1.)),
    ImageBackground::Black => Some(gpui_kit::hsla(0., 0., 0., 1.)),
    ImageBackground::Checkerboard => None,
  };
  if let Some(color) = solid {
    window.paint_quad(gpui_kit::fill(bounds, color));
    return;
  }
  let (light, dark) = checker;
  window.paint_quad(gpui_kit::fill(bounds, light));
  let cell = px(CHECKER_CELL);
  let columns = checker_steps(bounds.size.width);
  let rows = checker_steps(bounds.size.height);
  for row in 0..rows {
    for column in (usize::from(row % 2 == 0)..columns).step_by(2) {
      let origin = Point {
        x: bounds.origin.x + cell * checker_offset(column),
        y: bounds.origin.y + cell * checker_offset(row),
      };
      window.paint_quad(gpui_kit::fill(Bounds { origin, size: size(cell, cell) }, dark));
    }
  }
}

/// How many checkerboard cells cover `length`, bounded so one frame cannot
/// enqueue an unreasonable number of quads.
fn checker_steps(length: Pixels) -> usize {
  const MAX_CELLS: u32 = 4096;
  round_to_u32((f32::from(length) / CHECKER_CELL).ceil())
    .min(MAX_CELLS)
    .try_into()
    .unwrap_or(0)
}

/// A cell index as a multiplier for the cell size.
fn checker_offset(index: usize) -> f32 {
  f32::from(u16::try_from(index).unwrap_or(u16::MAX))
}

/// Device pixels as window pixels. Textures are capped well inside `i16`.
fn size_in_pixels(size: Size<DevicePixels>) -> Size<Pixels> {
  gpui_kit::size(px(pixels_of(size.width)), px(pixels_of(size.height)))
}

/// One device pixel count as an exact `f32`.
fn pixels_of(value: DevicePixels) -> f32 {
  f32::from(i16::try_from(value.0).unwrap_or(i16::MAX))
}

/// A scale as a whole percentage.
fn percent(scale: f32) -> u32 {
  round_to_u32(scale * 100.)
}

/// Round a finite, non-negative `f32` to the nearest `u32`, saturating.
#[expect(
  clippy::as_conversions,
  clippy::cast_possible_truncation,
  clippy::cast_sign_loss,
  reason = "no checked float-to-integer conversion exists; the value is clamped into range first"
)]
fn round_to_u32(value: f32) -> u32 {
  if !value.is_finite() || value <= 0. {
    return 0;
  }
  value.round().min(f32::from(u16::MAX)) as u32
}

/// A file size for the status bar.
pub(crate) fn format_size(bytes: u64) -> String {
  const KB: f64 = 1024.;
  const MB: f64 = KB * KB;
  let bytes_f = f64::from(u32::try_from(bytes.min(u64::from(u32::MAX))).unwrap_or(u32::MAX));
  if bytes_f >= MB {
    format!("{:.1} MB", bytes_f / MB)
  } else if bytes_f >= KB {
    format!("{:.1} KB", bytes_f / KB)
  } else {
    format!("{bytes} B")
  }
}

impl Render for ImageView {
  fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let theme = cx.theme();
    div()
      .key_context("ImageView")
      .track_focus(&self.focus)
      .on_action(cx.listener(Self::zoom_in))
      .on_action(cx.listener(Self::zoom_out))
      .on_action(cx.listener(Self::zoom_to_fit))
      .on_action(cx.listener(Self::actual_size))
      .on_action(cx.listener(Self::cycle_background))
      .on_action(cx.listener(Self::choose_background))
      .on_action(cx.listener(Self::open_theme_picker))
      .on_action(cx.listener(Self::open_nearby_picker))
      .on_action(cx.listener(Self::rotate_left))
      .on_action(cx.listener(Self::rotate_right))
      .on_action(cx.listener(Self::flip_horizontal))
      .on_action(cx.listener(Self::flip_vertical))
      .on_action(cx.listener(Self::save))
      .on_action(cx.listener(Self::close))
      .on_action(cx.listener(Self::open_export))
      .on_action(cx.listener(Self::undo))
      .on_action(cx.listener(Self::redo))
      .on_drop(cx.listener(|_, paths: &ExternalPaths, _, cx| apply_external_paths(paths, cx)))
      .drag_over::<ExternalPaths>(|style, _, _, cx| external_paths_ring(style, cx))
      .relative()
      .flex()
      .flex_col()
      .size_full()
      .bg(theme.background)
      .text_color(theme.foreground)
      .child(self.render_title_row(cx))
      .child(self.render_body(cx))
      .child(self.render_status_bar(cx))
      .children(self.theme_picker.clone().map(IntoElement::into_any_element))
      .children(self.nearby_picker.clone().map(IntoElement::into_any_element))
      .children(self.export_dialog.clone().map(IntoElement::into_any_element))
  }
}

#[cfg(test)]
mod tests {
  use gpui_kit::{KeyBinding, Point, TestAppContext, VisualTestContext, px};
  use openit_core::document::{LoadedImage, load_image};
  use openit_core::session::SessionId;

  use super::{ImageBackground, ImageView, SAVE_DELAY, format_size};
  use crate::actions::{ActualSize, CycleBackground, RotateRight, ZoomIn, ZoomToFit};
  use crate::document_view::tests::install_globals;
  use crate::session::CHECKPOINT_DELAY;

  fn write_png(dir: &std::path::Path, name: &str, width: u32, height: u32, alpha: u8) -> std::path::PathBuf {
    let path = dir.join(name);
    image::RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, alpha]))
      .save(&path)
      .unwrap();
    path
  }

  fn open(
    cx: &mut TestAppContext,
    path: std::path::PathBuf,
    loaded: LoadedImage,
  ) -> (gpui_kit::Entity<ImageView>, &mut VisualTestContext) {
    let (view, cx) = cx.add_window_view(|window, cx| ImageView::open(path, loaded, None, SessionId::new(), window, cx));
    cx.run_until_parked();
    (view, cx)
  }

  #[gpui_kit::test]
  fn an_opened_png_reports_its_metadata(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = write_png(doc.path(), "p.png", 64, 32, 255);
    let loaded = load_image(&path).unwrap();
    let expected = format!("64 x 32 · PNG · {}", format_size(std::fs::metadata(&path).unwrap().len()));

    let (view, cx) = open(cx, path, loaded);

    assert_eq!(view.read_with(cx, |view, _| view.status_text()), expected);
    assert_eq!(view.read_with(cx, |view, _| view.decode_error().map(str::to_owned)), None);
    assert_eq!(view.read_with(cx, |view, _| view.background()), ImageBackground::Theme);
  }

  #[gpui_kit::test]
  fn a_transparent_png_opens_on_the_checkerboard_and_cycles(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = write_png(doc.path(), "clear.png", 8, 8, 0);
    let loaded = load_image(&path).unwrap();

    let (view, cx) = open(cx, path, loaded);

    assert_eq!(view.read_with(cx, |view, _| view.background()), ImageBackground::Checkerboard);
    cx.dispatch_action(CycleBackground);
    assert_eq!(view.read_with(cx, |view, _| view.background()), ImageBackground::White);
    cx.dispatch_action(CycleBackground);
    assert_eq!(view.read_with(cx, |view, _| view.background()), ImageBackground::Black);
    cx.dispatch_action(CycleBackground);
    assert_eq!(view.read_with(cx, |view, _| view.background()), ImageBackground::Checkerboard);
  }

  #[gpui_kit::test]
  fn zoom_actions_move_the_percentage(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = write_png(doc.path(), "p.png", 64, 32, 255);
    let loaded = load_image(&path).unwrap();

    let (view, cx) = open(cx, path, loaded);

    cx.dispatch_action(ActualSize);
    assert_eq!(view.read_with(cx, |view, _| view.zoom_percent()), Some(100));
    cx.dispatch_action(ZoomIn);
    assert_eq!(view.read_with(cx, |view, _| view.zoom_percent()), Some(125));
    cx.dispatch_action(ZoomToFit);
    assert_eq!(
      view.read_with(cx, |view, _| view.zoom_percent()),
      Some(100),
      "an image smaller than the viewport fits at its own size"
    );
  }

  #[gpui_kit::test]
  fn an_svg_opens_on_the_checkerboard(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("logo.svg");
    std::fs::write(
      &path,
      br#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"></svg>"#,
    )
    .unwrap();
    let loaded = load_image(&path).unwrap();

    let (view, cx) = open(cx, path, loaded);

    assert_eq!(view.read_with(cx, |view, _| view.background()), ImageBackground::Checkerboard);
    assert!(view.read_with(cx, |view, _| view.status_text()).starts_with("20 x 10 · SVG"));
    assert_eq!(view.read_with(cx, |view, _| view.decode_error().map(str::to_owned)), None);
  }

  #[test]
  fn an_image_smaller_than_the_viewport_cannot_be_panned() {
    let viewport = gpui_kit::size(px(800.), px(600.));
    let drawn = gpui_kit::size(px(200.), px(100.));

    let clamped = super::clamp_pan(Point { x: px(-500.), y: px(320.) }, viewport, drawn);

    assert_eq!(clamped, Point { x: px(0.), y: px(0.) });
  }

  #[test]
  fn panning_stops_at_the_edge_of_a_larger_image() {
    let viewport = gpui_kit::size(px(800.), px(600.));
    let drawn = gpui_kit::size(px(1000.), px(1600.));

    // Half the overflow in each direction is as far as the image can travel.
    assert_eq!(
      super::clamp_pan(Point { x: px(9000.), y: px(9000.) }, viewport, drawn),
      Point { x: px(100.), y: px(500.) }
    );
    assert_eq!(
      super::clamp_pan(Point { x: px(-9000.), y: px(-9000.) }, viewport, drawn),
      Point { x: px(-100.), y: px(-500.) }
    );
    assert_eq!(
      super::clamp_pan(Point { x: px(40.), y: px(-60.) }, viewport, drawn),
      Point { x: px(40.), y: px(-60.) },
      "a pan inside the bounds is untouched"
    );
  }

  #[gpui_kit::test]
  fn dragging_a_fitted_image_leaves_it_centered(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = write_png(doc.path(), "p.png", 64, 32, 255);
    let loaded = load_image(&path).unwrap();

    let (view, cx) = open(cx, path, loaded);

    view.update(cx, |view, cx| view.pan_by(Point { x: px(400.), y: px(400.) }, cx));
    assert_eq!(view.read_with(cx, |view, _| view.pan()), Point { x: px(0.), y: px(0.) });
  }

  #[gpui_kit::test]
  fn a_corrupt_image_reports_the_failure_in_place(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = write_png(doc.path(), "p.png", 8, 8, 255);
    let mut loaded = load_image(&path).unwrap();
    loaded.bytes.truncate(40);

    let (view, cx) = open(cx, path, loaded);

    assert!(view.read_with(cx, |view, _| view.decode_error().is_some()));
  }

  #[test]
  fn sizes_read_in_kilobytes_and_megabytes() {
    assert_eq!(format_size(512), "512 B");
    assert_eq!(format_size(2048), "2.0 KB");
    assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
  }

  #[gpui_kit::test]
  fn rotating_writes_the_file_after_the_pause(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("p.png");
    let mut source = image::RgbaImage::new(2, 1);
    source.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
    source.put_pixel(1, 0, image::Rgba([0, 0, 255, 255]));
    source.save(&path).unwrap();
    let loaded = load_image(&path).unwrap();

    let (view, cx) = open(cx, path.clone(), loaded);
    cx.dispatch_action(RotateRight);
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, _| view.is_dirty()), "the write is still pending");

    cx.executor().advance_clock(SAVE_DELAY);
    cx.run_until_parked();

    let written = image::open(&path).unwrap().into_rgba8();
    assert_eq!(written.dimensions(), (1, 2));
    assert_eq!(written.get_pixel(0, 0).0, [255, 0, 0, 255]);
    assert_eq!(written.get_pixel(0, 1).0, [0, 0, 255, 255]);
    assert!(!view.read_with(cx, |view, _| view.is_dirty()), "the file is already up to date");
    assert_eq!(view.read_with(cx, |view, _| view.notice()), Some("Saved".to_owned()));
    assert!(store.list().unwrap().is_empty(), "a saved document needs no draft");
  }

  #[gpui_kit::test]
  fn undoing_a_rotation_restores_the_original_file(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = write_png(doc.path(), "p.png", 4, 2, 255);
    let loaded = load_image(&path).unwrap();
    let original = std::fs::read(&path).unwrap();

    let (view, cx) = open(cx, path.clone(), loaded);
    cx.dispatch_action(RotateRight);
    cx.executor().advance_clock(SAVE_DELAY);
    cx.run_until_parked();
    assert_ne!(std::fs::read(&path).unwrap(), original);

    cx.dispatch_action(gpui_kit::component::input::Undo);
    cx.executor().advance_clock(SAVE_DELAY);
    cx.run_until_parked();

    assert_eq!(std::fs::read(&path).unwrap(), original, "undo puts the original bytes back");
    assert_eq!(
      view.read_with(cx, |view, _| view.status_text()).split(" · ").next(),
      Some("4 x 2")
    );
  }

  #[gpui_kit::test]
  fn four_turns_leave_the_file_as_it_was(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = write_png(doc.path(), "p.png", 6, 3, 255);
    let loaded = load_image(&path).unwrap();
    let original = std::fs::read(&path).unwrap();

    let (_view, cx) = open(cx, path.clone(), loaded);
    for _ in 0..4 {
      cx.dispatch_action(RotateRight);
      cx.run_until_parked();
    }
    assert_eq!(
      std::fs::read(&path).unwrap(),
      original,
      "a burst of turns has not been written yet"
    );

    cx.executor().advance_clock(SAVE_DELAY);
    cx.run_until_parked();

    assert_eq!(std::fs::read(&path).unwrap(), original);
  }

  #[gpui_kit::test]
  fn discarding_a_clipboard_image_on_close_removes_its_draft(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    let mut png = Vec::new();
    image::RgbaImage::from_pixel(4, 4, image::Rgba([1, 2, 3, 255]))
      .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
      .unwrap();

    let (_view, cx) = cx.add_window_view(|window, cx| ImageView::from_clipboard(png, (4, 4), window, cx));
    cx.executor().advance_clock(CHECKPOINT_DELAY);
    cx.run_until_parked();
    assert_eq!(store.list().unwrap().len(), 1);

    assert!(!cx.simulate_close());
    cx.simulate_prompt_answer("Discard");
    cx.run_until_parked();

    assert!(store.list().unwrap().is_empty());
    assert_eq!(cx.windows().len(), 0);
  }

  #[gpui_kit::test]
  fn closing_writes_a_pending_turn_without_asking(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = write_png(doc.path(), "p.png", 4, 2, 255);
    let loaded = load_image(&path).unwrap();

    let (_view, cx) = open(cx, path.clone(), loaded);
    cx.dispatch_action(RotateRight);
    cx.run_until_parked();

    cx.dispatch_action(crate::actions::CloseWindow);
    cx.run_until_parked();

    assert!(!cx.has_pending_prompt(), "the turn was the decision; the close just writes it");
    assert_eq!(cx.windows().len(), 0);
    assert_eq!(image::open(&path).unwrap().into_rgba8().dimensions(), (2, 4));
    assert!(store.list().unwrap().is_empty());
  }

  #[gpui_kit::test]
  fn the_app_quit_hook_writes_a_pending_turn(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = write_png(doc.path(), "p.png", 4, 2, 255);
    let loaded = load_image(&path).unwrap();

    let (_view, cx) = open(cx, path.clone(), loaded);
    cx.dispatch_action(RotateRight);
    cx.run_until_parked();

    // What `App::on_app_quit` runs, whichever way the application is quit.
    let writes = cx.cx.update(crate::flush_pending_image_writes);
    assert_eq!(writes.len(), 1);
    for write in writes {
      cx.foreground_executor().block_test(write).unwrap();
    }

    assert_eq!(image::open(&path).unwrap().into_rgba8().dimensions(), (2, 4));
  }

  #[gpui_kit::test]
  fn quitting_writes_a_pending_turn(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = write_png(doc.path(), "p.png", 4, 2, 255);
    let loaded = load_image(&path).unwrap();

    let (_view, cx) = open(cx, path.clone(), loaded);
    cx.dispatch_action(RotateRight);
    cx.run_until_parked();

    let quit = cx.cx.update(crate::request_quit);
    cx.run_until_parked();

    assert!(cx.foreground_executor().block_test(quit));
    assert_eq!(image::open(&path).unwrap().into_rgba8().dimensions(), (2, 4));
  }

  #[gpui_kit::test]
  fn rotating_an_svg_stays_in_the_window(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("logo.svg");
    let source = br#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"></svg>"#;
    std::fs::write(&path, source).unwrap();
    let loaded = load_image(&path).unwrap();

    let (view, cx) = open(cx, path.clone(), loaded);
    cx.dispatch_action(RotateRight);
    cx.run_until_parked();

    assert_eq!(std::fs::read(&path).unwrap(), source, "the file is untouched");
    assert_eq!(
      view.read_with(cx, |view, _| view.status_text()).split(" · ").next(),
      Some("10 x 20"),
      "the window shows the turn"
    );
    let notice = view.read_with(cx, |view, _| view.notice());
    assert!(
      notice.is_some_and(|notice| notice.contains("Export")),
      "the window says where an SVG can be written"
    );
  }

  #[gpui_kit::test]
  fn a_clean_image_reloads_when_the_file_changes(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = write_png(doc.path(), "p.png", 64, 32, 255);
    let loaded = load_image(&path).unwrap();

    let (view, cx) = open(cx, path.clone(), loaded);
    image::RgbaImage::from_pixel(16, 48, image::Rgba([1, 2, 3, 255]))
      .save(&path)
      .unwrap();
    view.update(cx, ImageView::notify_disk_change_for_test);
    cx.run_until_parked();

    assert_eq!(
      view.read_with(cx, |view, _| view.status_text()).split(" · ").next(),
      Some("16 x 48")
    );
  }

  #[gpui_kit::test]
  fn quitting_with_a_clipboard_image_asks_and_discard_drops_the_draft(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    let mut png = Vec::new();
    image::RgbaImage::from_pixel(4, 4, image::Rgba([1, 2, 3, 255]))
      .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
      .unwrap();

    let (_view, cx) = cx.add_window_view(|window, cx| ImageView::from_clipboard(png, (4, 4), window, cx));
    cx.executor().advance_clock(CHECKPOINT_DELAY);
    cx.run_until_parked();
    assert_eq!(store.list().unwrap().len(), 1);

    let quit = cx.cx.update(crate::request_quit);
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
    assert!(!cx.foreground_executor().block_test(quit));
    assert_eq!(cx.windows().len(), 1);
    assert_eq!(store.list().unwrap().len(), 1);

    let quit = cx.cx.update(crate::request_quit);
    cx.run_until_parked();
    cx.simulate_prompt_answer("Discard");
    cx.run_until_parked();
    assert!(cx.foreground_executor().block_test(quit));
    assert!(store.list().unwrap().is_empty());
    assert_eq!(cx.windows().len(), 0);
  }

  #[gpui_kit::test]
  fn a_clipboard_image_opens_untitled_and_dirty(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    let mut png = Vec::new();
    image::RgbaImage::from_pixel(4, 4, image::Rgba([1, 2, 3, 255]))
      .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
      .unwrap();
    let (width, height) = (4, 4);

    let (view, cx) = cx.add_window_view(|window, cx| ImageView::from_clipboard(png, (width, height), window, cx));
    cx.run_until_parked();

    assert!(view.read_with(cx, |view, _| view.is_dirty()));
    assert_eq!(view.read_with(cx, |view, _| view.title()), "Untitled");
    assert_eq!(
      view.read_with(cx, |view, _| view.status_text()).split(" · ").nth(1),
      Some("PNG")
    );

    cx.executor().advance_clock(CHECKPOINT_DELAY);
    cx.run_until_parked();
    let draft = store.list().unwrap().into_iter().next().expect("the clipboard image is kept");
    assert!(draft.path.is_none());
    assert_eq!(
      store.read_blob(draft.session).unwrap().len(),
      view.read_with(cx, |view, _| view.byte_len())
    );
  }

  #[test]
  fn a_non_png_clipboard_image_becomes_png() {
    let mut jpeg = Vec::new();
    image::RgbImage::from_pixel(3, 2, image::Rgb([200, 100, 50]))
      .write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
      .unwrap();

    let (png, width, height) = openit_core::raster::to_png(&jpeg).unwrap();

    assert_eq!((width, height), (3, 2));
    assert_eq!(image::guess_format(&png).unwrap(), image::ImageFormat::Png);
  }

  #[gpui_kit::test]
  fn go_to_file_opens_the_nearby_picker(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    cx.update(|cx| cx.bind_keys([KeyBinding::new("cmd-p", crate::actions::GoToFile, None)]));
    let doc = tempfile::tempdir().unwrap();
    let path = write_png(doc.path(), "p.png", 8, 8, 255);
    let loaded = load_image(&path).unwrap();
    let (view, cx) = open(cx, path, loaded);

    cx.simulate_keystrokes("cmd-p");
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, _| view.nearby_picker.is_some()));
  }
}
