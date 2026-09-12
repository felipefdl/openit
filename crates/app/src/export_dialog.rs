//! The export dialog: format, size, background, and the advanced options.

use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{App, AppContext, Context, Entity, EventEmitter, IntoElement, Render, Subscription, Window, div, px};
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

/// The formats the dialog can write, in the order they are shown.
const FORMATS: [OutputFormat; 6] = [
  OutputFormat::Png,
  OutputFormat::Jpeg { quality: 85 },
  OutputFormat::WebP,
  OutputFormat::Bmp,
  OutputFormat::Tiff,
  OutputFormat::Gif,
];

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
  _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ExportEvent> for ExportDialog {}

impl ExportDialog {
  /// Open the dialog for a source of `source` size.
  pub fn new(source: (u32, u32), has_alpha: bool, stem: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let (width, height) = (source.0.max(1), source.1.max(1));
    let width_input = cx.new(|cx| InputState::new(window, cx).default_value(width.to_string()));
    let height_input = cx.new(|cx| InputState::new(window, cx).default_value(height.to_string()));
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
      quality: 85,
      width_input,
      height_input,
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

  /// Fill the size fields from a preset.
  pub fn set_preset(&mut self, preset: Preset, window: &mut Window, cx: &mut Context<Self>) {
    self.preset = preset;
    if let Some((width, height)) = preset.size(self.source) {
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
    if rule == SizeRule::FitInside && self.lock_aspect {
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
      .child(div().w(px(84.)).text_sm().text_color(muted).child(label))
      .child(control)
      .into_any_element()
  }

  /// Format, size preset, pixel fields, and background.
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
          .child(div().w(px(96.)).child(Input::new(&self.width_input).small()))
          .child(div().text_xs().text_color(muted).child("x"))
          .child(div().w(px(96.)).child(Input::new(&self.height_input).small()))
          .child(
            Checkbox::new("export-lock")
              .label("Lock aspect")
              .checked(self.lock_aspect)
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

  /// The collapsed section: sizing rule and metadata.
  #[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "cx.listener requires the mutable Context signature"
  )]
  fn render_advanced(&self, muted: gpui_kit::Hsla, cx: &mut Context<Self>) -> Vec<gpui_kit::AnyElement> {
    let toggle = div()
      .id("export-advanced")
      .px_3()
      .py_2()
      .text_xs()
      .text_color(muted)
      .cursor_pointer()
      .child(if self.advanced_open {
        "Advanced"
      } else {
        "Advanced..."
      })
      .on_click(cx.listener(|dialog, _, _window, cx| {
        dialog.advanced_open = !dialog.advanced_open;
        cx.notify();
      }))
      .into_any_element();
    let mut rows = vec![toggle];
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
    rows
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
  fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let palette = cx.global::<ActivePalette>().0;
    let muted = cx.theme().muted_foreground;
    let mut children: Vec<gpui_kit::AnyElement> = vec![
      div()
        .px_3()
        .pt_3()
        .pb_1()
        .text_sm()
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .child("Export image")
        .into_any_element(),
    ];
    children.extend(self.render_choices(muted, cx));
    children.extend(self.render_advanced(muted, cx));
    children.push(self.render_footer(muted, cx));
    overlay_frame(
      "export-dialog",
      &palette,
      cx.listener(|_, _, _, cx| cx.emit(ExportEvent::Close)),
      move |panel| panel.children(children),
    )
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
