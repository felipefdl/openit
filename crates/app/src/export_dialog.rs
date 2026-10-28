//! The export dialog: format, size, background, and the advanced options.

use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::{ActiveTheme, Disableable, Icon, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{
  App, AppContext, Context, Entity, EventEmitter, FocusHandle, IntoElement, Render, Subscription, Window, div, px,
};
use openit_core::raster::{Background, ExportOptions, OutputFormat, SizeRule, output_size};

use crate::status_pickers::overlay_frame;
use crate::theme::ActivePalette;

/// The size presets the dialog offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
  /// The source size.
  Original,
  /// Half the source size.
  Half,
  /// Fit inside 1920 x 1080.
  P1080,
  /// Whatever the fields say.
  Custom,
}

impl Preset {
  const ALL: [Self; 4] = [Self::Original, Self::Half, Self::P1080, Self::Custom];

  const fn label(self) -> &'static str {
    match self {
      Self::Original => "Original",
      Self::Half => "50%",
      Self::P1080 => "1080p",
      Self::Custom => "Custom",
    }
  }

  /// The size this preset asks for, given the source size.
  fn size(self, source: (u32, u32)) -> Option<(u32, u32)> {
    match self {
      Self::Original => Some(source),
      Self::Half => Some(((source.0 / 2).max(1), (source.1 / 2).max(1))),
      Self::P1080 => Some((1920, 1080)),
      Self::Custom => None,
    }
  }
}

/// JPEG quality the dialog starts at.
const DEFAULT_QUALITY: u8 = 85;

/// The formats the dialog can write, in the order they are shown.
const FORMATS: [OutputFormat; 6] = [
  OutputFormat::Png,
  OutputFormat::Jpeg { quality: DEFAULT_QUALITY },
  OutputFormat::WebP,
  OutputFormat::Bmp,
  OutputFormat::Tiff,
  OutputFormat::Gif,
];

/// Width of the dialog panel; wide enough for every format on one row.
const PANEL_WIDTH: f32 = 560.;
/// Gap above the panel, matching `overlay_frame`.
const PANEL_TOP: f32 = 60.;
/// Gap kept below the panel: the image window's status bar row (24 px) plus a
/// margin, so a short window still shows the panel's edge above that bar.
const PANEL_BOTTOM: f32 = 40.;
/// Height reserved for the two advanced rows, so opening them moves nothing.
const ADVANCED_HEIGHT: f32 = 80.;

/// What the dialog decided.
pub enum ExportEvent {
  /// Write the image with these options.
  Confirm(ExportOptions),
  /// Dismissed without exporting.
  Close,
}

/// Format, size, background, and the advanced options for one export.
pub struct ExportDialog {
  source: (u32, u32),
  stem: String,
  format: OutputFormat,
  preset: Preset,
  width: u32,
  height: u32,
  lock_aspect: bool,
  background: Background,
  advanced_open: bool,
  size_rule: SizeRule,
  keep_icc: bool,
  quality: u8,
  width_input: Entity<InputState>,
  height_input: Entity<InputState>,
  quality_input: Entity<InputState>,
  focus: FocusHandle,
  _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ExportEvent> for ExportDialog {}

impl ExportDialog {
  /// Open the dialog for a source of `source` size.
  pub fn new(source: (u32, u32), has_alpha: bool, stem: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let (width, height) = (source.0.max(1), source.1.max(1));
    let width_input = cx.new(|cx| InputState::new(window, cx).default_value(width.to_string()));
    let height_input = cx.new(|cx| InputState::new(window, cx).default_value(height.to_string()));
    let quality_input = cx.new(|cx| InputState::new(window, cx).default_value(DEFAULT_QUALITY.to_string()));
    let focus = cx.focus_handle();
    window.focus(&focus, cx);
    let subscriptions = vec![
      cx.subscribe_in(
        &width_input,
        window,
        |dialog: &mut Self, state, event: &InputEvent, window, cx| {
          if matches!(event, InputEvent::Change)
            && let Ok(typed) = state.read(cx).value().parse::<u32>()
            && typed > 0
          {
            dialog.set_width(typed, window, cx);
          }
        },
      ),
      cx.subscribe_in(
        &height_input,
        window,
        |dialog: &mut Self, state, event: &InputEvent, window, cx| {
          if matches!(event, InputEvent::Change)
            && let Ok(typed) = state.read(cx).value().parse::<u32>()
            && typed > 0
          {
            dialog.set_height(typed, window, cx);
          }
        },
      ),
      cx.subscribe_in(
        &quality_input,
        window,
        |dialog: &mut Self, state, event: &InputEvent, _window, cx| {
          if matches!(event, InputEvent::Change)
            && let Ok(typed) = state.read(cx).value().trim().parse::<u8>()
            && (1..=100).contains(&typed)
          {
            dialog.set_quality(typed, cx);
          }
        },
      ),
    ];
    Self {
      source: (width, height),
      stem: stem.to_owned(),
      format: OutputFormat::Png,
      preset: Preset::Original,
      width,
      height,
      lock_aspect: true,
      background: if has_alpha {
        Background::Transparent
      } else {
        Background::White
      },
      advanced_open: false,
      size_rule: SizeRule::FitInside,
      keep_icc: false,
      quality: DEFAULT_QUALITY,
      width_input,
      height_input,
      quality_input,
      focus,
      _subscriptions: subscriptions,
    }
  }

  /// Which preset is selected.
  pub const fn preset(&self) -> Preset {
    self.preset
  }

  /// The file name to suggest in the save prompt.
  pub fn suggested_name(&self) -> String {
    format!("{}.{}", self.stem, self.format.extension())
  }

  /// Everything the export needs.
  pub const fn options(&self, _cx: &App) -> ExportOptions {
    ExportOptions {
      format: self.format,
      width: self.width,
      height: self.height,
      size_rule: self.size_rule,
      background: self.background,
      keep_icc: self.keep_icc,
    }
  }

  /// The pixel size the export will produce.
  pub fn output_size(&self, cx: &App) -> (u32, u32) {
    output_size(self.source, &self.options(cx))
  }

  /// Choose the output format, dropping transparency the format cannot keep.
  pub fn set_format(&mut self, format: OutputFormat, cx: &mut Context<Self>) {
    self.format = match format {
      OutputFormat::Jpeg { .. } => OutputFormat::Jpeg { quality: self.quality },
      other => other,
    };
    if !self.format.supports_alpha() && self.background == Background::Transparent {
      self.background = Background::White;
    }
    cx.notify();
  }

  /// Choose the JPEG quality, 1 through 100.
  pub fn set_quality(&mut self, quality: u8, cx: &mut Context<Self>) {
    self.quality = quality.clamp(1, 100);
    if let OutputFormat::Jpeg { .. } = self.format {
      self.format = OutputFormat::Jpeg { quality: self.quality };
    }
    cx.notify();
  }

  /// Fill the size fields from a preset.
  pub fn set_preset(&mut self, preset: Preset, window: &mut Window, cx: &mut Context<Self>) {
    self.preset = preset;
    self.apply_preset(window, cx);
  }

  /// Write the size the current preset asks for into the fields. `1080p`
  /// under `Fit inside` shows the size the export will produce, so the fields
  /// and the readout agree.
  fn apply_preset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if let Some((width, height)) = self.preset.size(self.source) {
      let (width, height) = if self.preset == Preset::P1080 && self.size_rule == SizeRule::FitInside {
        output_size(self.source, &ExportOptions { width, height, ..self.options(cx) })
      } else {
        (width, height)
      };
      self.write_size(width, height, window, cx);
    }
    cx.notify();
  }

  /// Set the requested width, keeping the ratio when the aspect is locked.
  pub fn set_width(&mut self, width: u32, window: &mut Window, cx: &mut Context<Self>) {
    self.preset = Preset::Custom;
    let height = self.paired_height(width);
    self.write_size(width, height, window, cx);
  }

  /// Set the requested height, keeping the ratio when the aspect is locked.
  pub fn set_height(&mut self, height: u32, window: &mut Window, cx: &mut Context<Self>) {
    self.preset = Preset::Custom;
    let width = self.paired_width(height);
    self.write_size(width, height, window, cx);
  }

  /// Choose how the requested size is read.
  pub fn set_size_rule(&mut self, rule: SizeRule, window: &mut Window, cx: &mut Context<Self>) {
    self.size_rule = rule;
    if self.preset == Preset::P1080 {
      self.apply_preset(window, cx);
    } else if rule == SizeRule::FitInside && self.lock_aspect {
      let height = self.paired_height(self.width);
      self.write_size(self.width, height, window, cx);
    }
    cx.notify();
  }

  /// Choose what fills transparent pixels.
  pub fn set_background(&mut self, background: Background, cx: &mut Context<Self>) {
    self.background = background;
    cx.notify();
  }

  /// Carry the source color profile into the output, or drop it.
  pub fn set_keep_icc(&mut self, keep: bool, cx: &mut Context<Self>) {
    self.keep_icc = keep;
    cx.notify();
  }

  /// The height that pairs with `width` under the current rules.
  fn paired_height(&self, width: u32) -> u32 {
    if !self.lock_aspect || self.size_rule == SizeRule::Exact {
      return self.height;
    }
    scaled(self.source.1, self.source.0, width)
  }

  /// The width that pairs with `height` under the current rules.
  fn paired_width(&self, height: u32) -> u32 {
    if !self.lock_aspect || self.size_rule == SizeRule::Exact {
      return self.width;
    }
    scaled(self.source.0, self.source.1, height)
  }

  /// Store a size and mirror it into the input fields.
  fn write_size(&mut self, width: u32, height: u32, window: &mut Window, cx: &mut Context<Self>) {
    self.width = width.max(1);
    self.height = height.max(1);
    let (width_text, height_text) = (self.width.to_string(), self.height.to_string());
    self.width_input.update(cx, |state, cx| {
      if state.value() != width_text {
        state.set_value(&width_text, window, cx);
      }
    });
    self.height_input.update(cx, |state, cx| {
      if state.value() != height_text {
        state.set_value(&height_text, window, cx);
      }
    });
    cx.notify();
  }
}

/// One axis scaled to `target` on the other axis.
fn scaled(other: u32, primary: u32, target: u32) -> u32 {
  let value = u64::from(other)
    .saturating_mul(u64::from(target))
    .saturating_add(u64::from(primary.max(1)) / 2)
    / u64::from(primary.max(1));
  u32::try_from(value).unwrap_or(u32::MAX).max(1)
}

impl ExportDialog {
  /// One labelled row of the dialog.
  fn row(label: &'static str, muted: gpui_kit::Hsla, control: gpui_kit::AnyElement) -> gpui_kit::AnyElement {
    div()
      .flex()
      .items_center()
      .gap_3()
      .px_3()
      .py_2()
      .child(div().w(px(84.)).flex_shrink_0().text_sm().text_color(muted).child(label))
      .child(control)
      .into_any_element()
  }

  /// Format, quality, size preset, pixel fields, and background.
  #[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "cx.listener requires the mutable Context signature"
  )]
  fn render_choices(&self, muted: gpui_kit::Hsla, cx: &mut Context<Self>) -> Vec<gpui_kit::AnyElement> {
    let format_index = FORMATS
      .iter()
      .position(|format| std::mem::discriminant(format) == std::mem::discriminant(&self.format));
    let backgrounds: Vec<(&'static str, Background)> = if self.format.supports_alpha() {
      vec![
        ("Transparent", Background::Transparent),
        ("White", Background::White),
        ("Black", Background::Black),
      ]
    } else {
      vec![("White", Background::White), ("Black", Background::Black)]
    };
    let background_index = backgrounds.iter().position(|(_, value)| *value == self.background);
    let background_values: Vec<Background> = backgrounds.iter().map(|(_, value)| *value).collect();
    // The row stays in place for every format: only JPEG has a quality.
    let jpeg = matches!(self.format, OutputFormat::Jpeg { .. });
    let size_locked = self.preset == Preset::Original;
    vec![
      Self::row(
        "Format",
        muted,
        RadioGroup::horizontal("export-format")
          .children(FORMATS.map(OutputFormat::label))
          .selected_index(format_index)
          .on_change(cx.listener(|dialog, index: &usize, _window, cx| {
            if let Some(format) = FORMATS.get(*index) {
              dialog.set_format(*format, cx);
            }
          }))
          .into_any_element(),
      ),
      Self::row(
        "Quality",
        muted,
        div()
          .flex()
          .items_center()
          .gap_2()
          .child(div().w(px(96.)).child(Input::new(&self.quality_input).small().disabled(!jpeg)))
          .child(div().text_xs().text_color(muted).child("1 to 100, JPEG only"))
          .into_any_element(),
      ),
      Self::row(
        "Size",
        muted,
        RadioGroup::horizontal("export-preset")
          .children(Preset::ALL.map(Preset::label))
          .selected_index(Preset::ALL.iter().position(|preset| *preset == self.preset()))
          .on_change(cx.listener(|dialog, index: &usize, window, cx| {
            if let Some(preset) = Preset::ALL.get(*index) {
              dialog.set_preset(*preset, window, cx);
            }
          }))
          .into_any_element(),
      ),
      Self::row(
        "Pixels",
        muted,
        div()
          .flex()
          .items_center()
          .gap_2()
          .child(
            div()
              .w(px(96.))
              .child(Input::new(&self.width_input).small().disabled(size_locked)),
          )
          .child(div().text_xs().text_color(muted).child("x"))
          .child(
            div()
              .w(px(96.))
              .child(Input::new(&self.height_input).small().disabled(size_locked)),
          )
          .child(
            Checkbox::new("export-lock")
              .label("Lock aspect")
              .checked(self.lock_aspect)
              .disabled(size_locked)
              .on_click(cx.listener(|dialog, checked: &bool, _window, cx| {
                dialog.lock_aspect = *checked;
                cx.notify();
              })),
          )
          .into_any_element(),
      ),
      Self::row(
        "Background",
        muted,
        RadioGroup::horizontal("export-background")
          .children(backgrounds.iter().map(|(label, _)| *label))
          .selected_index(background_index)
          .on_change(cx.listener(move |dialog, index: &usize, _window, cx| {
            if let Some(background) = background_values.get(*index) {
              dialog.set_background(*background, cx);
            }
          }))
          .into_any_element(),
      ),
    ]
  }

  /// The collapsed section: sizing rule and metadata. Its rows live in a box
  /// of fixed height, so opening it does not move the footer.
  #[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "cx.listener requires the mutable Context signature"
  )]
  fn render_advanced(&self, muted: gpui_kit::Hsla, cx: &mut Context<Self>) -> Vec<gpui_kit::AnyElement> {
    let toggle = div()
      .id("export-advanced")
      .flex()
      .items_center()
      .gap_1()
      .px_3()
      .py_2()
      .text_xs()
      .text_color(muted)
      .cursor_pointer()
      .child(
        Icon::new(if self.advanced_open {
          IconName::ChevronDown
        } else {
          IconName::ChevronRight
        })
        .small(),
      )
      .child("Advanced")
      .on_click(cx.listener(|dialog, _, _window, cx| {
        dialog.advanced_open = !dialog.advanced_open;
        cx.notify();
      }))
      .into_any_element();
    let mut rows = Vec::new();
    if self.advanced_open {
      rows.push(Self::row(
        "Sizing",
        muted,
        RadioGroup::horizontal("export-sizing")
          .children(["Fit inside", "Exact size"])
          .selected_index(Some(usize::from(self.size_rule == SizeRule::Exact)))
          .on_change(cx.listener(|dialog, index: &usize, window, cx| {
            let rule = if *index == 1 {
              SizeRule::Exact
            } else {
              SizeRule::FitInside
            };
            dialog.set_size_rule(rule, window, cx);
          }))
          .into_any_element(),
      ));
      rows.push(Self::row(
        "Metadata",
        muted,
        Checkbox::new("export-icc")
          .label("Keep the color profile")
          .checked(self.keep_icc)
          .on_click(cx.listener(|dialog, checked: &bool, _window, cx| dialog.set_keep_icc(*checked, cx)))
          .into_any_element(),
      ));
    }
    vec![
      toggle,
      div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .h(px(ADVANCED_HEIGHT))
        .children(rows)
        .into_any_element(),
    ]
  }

  /// The output size readout and the two buttons.
  #[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "cx.listener requires the mutable Context signature"
  )]
  fn render_footer(&self, muted: gpui_kit::Hsla, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
    let (width, height) = self.output_size(cx);
    let border = cx.theme().border;
    div()
      .flex()
      .flex_none()
      .items_center()
      .gap_2()
      .px_3()
      .py_2()
      .border_t_1()
      .border_color(border)
      .child(
        div()
          .flex_1()
          .text_xs()
          .text_color(muted)
          .child(format!("Output: {width} x {height}")),
      )
      .child(
        Button::new("export-cancel")
          .label("Cancel")
          .small()
          .on_click(cx.listener(|_, _, _window, cx| cx.emit(ExportEvent::Close))),
      )
      .child(
        Button::new("export-confirm")
          .label("Export...")
          .with_variant(ButtonVariant::Primary)
          .small()
          .on_click(cx.listener(|dialog, _, _window, cx| {
            let options = dialog.options(cx);
            cx.emit(ExportEvent::Confirm(options));
          })),
      )
      .into_any_element()
  }
}

impl Render for ExportDialog {
  fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let palette = cx.global::<ActivePalette>().0;
    let muted = cx.theme().muted_foreground;
    let mut body = self.render_choices(muted, cx);
    body.extend(self.render_advanced(muted, cx));
    let header = div()
      .flex_none()
      .px_3()
      .pt_3()
      .pb_1()
      .text_sm()
      .font_weight(gpui_kit::FontWeight::SEMIBOLD)
      .child("Export image")
      .into_any_element();
    let footer = self.render_footer(muted, cx);
    // The options scroll inside the panel, so the footer stays reachable in a
    // window shorter than the dialog.
    let max_height = (window.viewport_size().height - px(PANEL_TOP + PANEL_BOTTOM)).max(px(160.));
    overlay_frame(
      "export-dialog",
      &palette,
      cx.listener(|_, _, _, cx| cx.emit(ExportEvent::Close)),
      move |panel| {
        panel
          .w(px(PANEL_WIDTH))
          .max_w_full()
          .max_h(max_height)
          .child(header)
          .child(
            div()
              .id("export-body")
              .flex()
              .flex_col()
              .flex_1()
              .min_h_0()
              .overflow_y_scroll()
              .children(body),
          )
          .child(footer)
      },
    )
    .track_focus(&self.focus)
    .capture_action(cx.listener(|_, _: &gpui_kit::component::input::Escape, _, cx| cx.emit(ExportEvent::Close)))
  }
}

#[cfg(test)]
mod tests {
  use gpui_kit::TestAppContext;
  use openit_core::raster::{Background, OutputFormat, SizeRule};

  use super::{ExportDialog, Preset};
  use crate::document_view::tests::install_globals;

  #[gpui_kit::test]
  fn a_preset_fills_the_size_and_editing_switches_to_custom(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let (dialog, cx) = cx.add_window_view(|window, cx| ExportDialog::new((4000, 2000), true, "photo", window, cx));

    dialog.update_in(cx, |dialog, window, cx| dialog.set_preset(Preset::P1080, window, cx));
    assert_eq!(dialog.read_with(cx, super::ExportDialog::output_size), (1920, 960));

    dialog.update_in(cx, |dialog, window, cx| dialog.set_width(320, window, cx));
    assert_eq!(
      dialog.read_with(cx, super::ExportDialog::output_size),
      (320, 160),
      "the aspect lock keeps the ratio"
    );
    assert_eq!(dialog.read_with(cx, |dialog, _| dialog.preset()), Preset::Custom);
  }

  #[gpui_kit::test]
  fn the_1080p_fields_show_the_size_the_export_produces(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let (dialog, cx) = cx.add_window_view(|window, cx| ExportDialog::new((256, 256), false, "icon", window, cx));

    dialog.update_in(cx, |dialog, window, cx| dialog.set_preset(Preset::P1080, window, cx));
    let options = dialog.read_with(cx, super::ExportDialog::options);
    let output = dialog.read_with(cx, super::ExportDialog::output_size);
    assert_eq!((options.width, options.height), output, "fields and readout agree");
    assert_eq!(output, (1080, 1080));

    dialog.update_in(cx, |dialog, window, cx| dialog.set_size_rule(SizeRule::Exact, window, cx));
    let options = dialog.read_with(cx, super::ExportDialog::options);
    assert_eq!((options.width, options.height), (1920, 1080), "exact keeps the preset as named");
    assert_eq!(dialog.read_with(cx, super::ExportDialog::output_size), (1920, 1080));
  }

  #[gpui_kit::test]
  fn exact_sizing_uses_both_fields(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let (dialog, cx) = cx.add_window_view(|window, cx| ExportDialog::new((1920, 1080), false, "shot", window, cx));

    dialog.update_in(cx, |dialog, window, cx| {
      dialog.set_size_rule(SizeRule::Exact, window, cx);
      dialog.set_width(320, window, cx);
      dialog.set_height(320, window, cx);
    });

    assert_eq!(dialog.read_with(cx, super::ExportDialog::output_size), (320, 320));
  }

  #[gpui_kit::test]
  fn a_format_without_alpha_drops_transparency(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let (dialog, cx) = cx.add_window_view(|window, cx| ExportDialog::new((10, 10), true, "logo", window, cx));

    assert_eq!(
      dialog.read_with(cx, |dialog, cx| dialog.options(cx).background),
      Background::Transparent,
      "a transparent source starts transparent"
    );

    dialog.update(cx, |dialog, cx| dialog.set_format(OutputFormat::Jpeg { quality: 85 }, cx));

    assert_eq!(
      dialog.read_with(cx, |dialog, cx| dialog.options(cx).background),
      Background::White
    );
  }

  #[gpui_kit::test]
  fn the_jpeg_quality_reaches_the_result_and_survives_a_format_change(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let (dialog, cx) = cx.add_window_view(|window, cx| ExportDialog::new((10, 10), false, "photo", window, cx));

    dialog.update(cx, |dialog, cx| {
      dialog.set_quality(60, cx);
      dialog.set_format(OutputFormat::Jpeg { quality: 85 }, cx);
    });
    assert_eq!(
      dialog.read_with(cx, |dialog, cx| dialog.options(cx).format),
      OutputFormat::Jpeg { quality: 60 }
    );

    dialog.update(cx, |dialog, cx| {
      dialog.set_format(OutputFormat::Png, cx);
      dialog.set_quality(0, cx);
      dialog.set_format(OutputFormat::Jpeg { quality: 85 }, cx);
    });
    assert_eq!(
      dialog.read_with(cx, |dialog, cx| dialog.options(cx).format),
      OutputFormat::Jpeg { quality: 1 },
      "quality is kept inside 1 to 100"
    );
  }

  #[gpui_kit::test]
  fn an_opaque_source_starts_on_white(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let (dialog, cx) = cx.add_window_view(|window, cx| ExportDialog::new((10, 10), false, "photo", window, cx));

    assert_eq!(
      dialog.read_with(cx, |dialog, cx| dialog.options(cx).background),
      Background::White
    );
  }

  #[gpui_kit::test]
  fn the_suggested_name_follows_the_format(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let (dialog, cx) = cx.add_window_view(|window, cx| ExportDialog::new((10, 10), false, "photo", window, cx));

    assert_eq!(dialog.read_with(cx, |dialog, _| dialog.suggested_name()), "photo.png");

    dialog.update(cx, |dialog, cx| dialog.set_format(OutputFormat::Jpeg { quality: 85 }, cx));

    assert_eq!(dialog.read_with(cx, |dialog, _| dialog.suggested_name()), "photo.jpg");
  }

  #[gpui_kit::test]
  fn advanced_options_reach_the_result(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let (dialog, cx) = cx.add_window_view(|window, cx| ExportDialog::new((100, 50), false, "photo", window, cx));

    dialog.update(cx, |dialog, cx| dialog.set_keep_icc(true, cx));
    let options = dialog.read_with(cx, super::ExportDialog::options);

    assert!(options.keep_icc);
    assert_eq!(options.size_rule, SizeRule::FitInside);
    assert_eq!(options.format, OutputFormat::Png);
  }
}
