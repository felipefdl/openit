//! Pixel work shared by the viewer, saving, and export: decoding with
//! orientation applied, rotation and flips, resizing, background compositing,
//! and encoding. No GPUI types cross this boundary.

use std::io::Cursor;

use image::{AnimationDecoder, ImageDecoder as _, ImageEncoder as _};
use serde::{Deserialize, Serialize};

use crate::document::{ImageFormat, decode_limits, probe_image};
use crate::error::Error;
use crate::kind::DocumentKind;

/// Quarter-turn rotation plus flips. Flips apply first, then the rotation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transform {
  /// Clockwise quarter turns, 0 through 3.
  pub quarter_turns: u8,
  /// Mirror left to right.
  pub flip_h: bool,
  /// Mirror top to bottom.
  pub flip_v: bool,
}

impl Transform {
  /// The image as it was decoded.
  pub const IDENTITY: Self = Self {
    quarter_turns: 0,
    flip_h: false,
    flip_v: false,
  };

  /// Whether the image is shown as decoded.
  pub const fn is_identity(self) -> bool {
    self.quarter_turns == 0 && !self.flip_h && !self.flip_v
  }

  /// One more quarter turn clockwise.
  #[must_use]
  pub const fn rotate_cw(self) -> Self {
    Self {
      quarter_turns: (self.quarter_turns + 1) % 4,
      ..self
    }
  }

  /// One more quarter turn counter-clockwise.
  #[must_use]
  pub const fn rotate_ccw(self) -> Self {
    Self {
      quarter_turns: (self.quarter_turns + 3) % 4,
      ..self
    }
  }

  /// Mirror left to right.
  #[must_use]
  pub const fn flip_horizontal(self) -> Self {
    Self { flip_h: !self.flip_h, ..self }
  }

  /// Mirror top to bottom.
  #[must_use]
  pub const fn flip_vertical(self) -> Self {
    Self { flip_v: !self.flip_v, ..self }
  }

  /// The size after this transform; odd quarter turns swap the axes.
  pub const fn apply_to_size(self, width: u32, height: u32) -> (u32, u32) {
    if self.quarter_turns.is_multiple_of(2) {
      (width, height)
    } else {
      (height, width)
    }
  }
}

/// A decoded still image with EXIF orientation already applied.
pub struct Decoded {
  /// Pixels, upright.
  pub image: image::DynamicImage,
  /// Embedded color profile, when the format carries one.
  pub icc: Option<Vec<u8>>,
  /// Whether the source format carries an alpha channel.
  pub has_alpha: bool,
}

/// A format `export` can write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
  /// PNG.
  Png,
  /// JPEG at the given quality, 1 through 100.
  Jpeg {
    /// Encoder quality.
    quality: u8,
  },
  /// Lossless WebP.
  WebP,
  /// Windows bitmap.
  Bmp,
  /// TIFF.
  Tiff,
  /// GIF, keeping every frame of an animation.
  Gif,
}

impl OutputFormat {
  /// The file extension to suggest in the save prompt.
  pub const fn extension(self) -> &'static str {
    match self {
      Self::Png => "png",
      Self::Jpeg { .. } => "jpg",
      Self::WebP => "webp",
      Self::Bmp => "bmp",
      Self::Tiff => "tiff",
      Self::Gif => "gif",
    }
  }

  /// Whether the format stores transparency.
  pub const fn supports_alpha(self) -> bool {
    matches!(self, Self::Png | Self::WebP | Self::Gif | Self::Tiff)
  }

  /// Upper-case label for the dialog.
  pub const fn label(self) -> &'static str {
    match self {
      Self::Png => "PNG",
      Self::Jpeg { .. } => "JPEG",
      Self::WebP => "WebP",
      Self::Bmp => "BMP",
      Self::Tiff => "TIFF",
      Self::Gif => "GIF",
    }
  }
}

/// What fills the pixels an image does not cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Background {
  /// Keep the alpha channel.
  Transparent,
  /// Composite over white.
  White,
  /// Composite over black.
  Black,
}

/// How the requested width and height are interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeRule {
  /// Scale to fit inside the box, keeping the aspect ratio.
  FitInside,
  /// Use the width and height exactly.
  Exact,
}

/// Everything the export dialog decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportOptions {
  /// Encoder to use.
  pub format: OutputFormat,
  /// Requested width.
  pub width: u32,
  /// Requested height.
  pub height: u32,
  /// How to read the requested size.
  pub size_rule: SizeRule,
  /// What fills transparent pixels.
  pub background: Background,
  /// Whether to carry the source color profile into the output.
  pub keep_icc: bool,
}

/// Decode one still image, applying EXIF orientation.
pub fn decode_still(bytes: &[u8], format: ImageFormat) -> Result<Decoded, Error> {
  let raster = format.raster().ok_or_else(|| unsupported(format))?;
  let mut reader = image::ImageReader::with_format(Cursor::new(bytes), raster);
  reader.limits(decode_limits());
  let mut decoder = reader.into_decoder().map_err(|error| pixel_error(&error))?;
  let orientation = decoder.orientation().map_err(|error| pixel_error(&error))?;
  let icc = decoder.icc_profile().map_err(|error| pixel_error(&error))?;
  let has_alpha = decoder.color_type().has_alpha();
  let mut image = image::DynamicImage::from_decoder(decoder).map_err(|error| pixel_error(&error))?;
  image.apply_orientation(orientation);
  Ok(Decoded { image, icc, has_alpha })
}

/// Decode every frame, stopping once `max_pixels` have been collected.
/// Still formats yield one frame. The bool is the decoder color type's alpha, not a pixel scan.
pub fn decode_frames(bytes: &[u8], format: ImageFormat, max_pixels: u64) -> Result<(Vec<image::Frame>, bool), Error> {
  match format {
    ImageFormat::Gif => {
      let mut decoder = image::codecs::gif::GifDecoder::new(Cursor::new(bytes)).map_err(|error| pixel_error(&error))?;
      decoder.set_limits(decode_limits()).map_err(|error| pixel_error(&error))?;
      let has_alpha = decoder.color_type().has_alpha();
      Ok((collect_frames(decoder, max_pixels)?, has_alpha))
    },
    ImageFormat::WebP => {
      let mut decoder =
        image::codecs::webp::WebPDecoder::new(Cursor::new(bytes)).map_err(|error| pixel_error(&error))?;
      decoder.set_limits(decode_limits()).map_err(|error| pixel_error(&error))?;
      let has_alpha = decoder.color_type().has_alpha();
      if decoder.has_animation() {
        Ok((collect_frames(decoder, max_pixels)?, has_alpha))
      } else {
        still_frame(bytes, format)
      }
    },
    ImageFormat::Png => {
      let mut decoder = image::codecs::png::PngDecoder::new(Cursor::new(bytes)).map_err(|error| pixel_error(&error))?;
      decoder.set_limits(decode_limits()).map_err(|error| pixel_error(&error))?;
      let has_alpha = decoder.color_type().has_alpha();
      if decoder.is_apng().map_err(|error| pixel_error(&error))? {
        let apng = decoder.apng().map_err(|error| pixel_error(&error))?;
        Ok((collect_frames(apng, max_pixels)?, has_alpha))
      } else {
        still_frame(bytes, format)
      }
    },
    other => still_frame(bytes, other),
  }
}

/// Rotate and flip one buffer. Identity returns `image` without copying.
#[must_use]
pub fn transformed(image: image::DynamicImage, transform: Transform) -> image::DynamicImage {
  if transform.is_identity() {
    return image;
  }
  let mut image = image;
  if transform.flip_h {
    image::imageops::flip_horizontal_in_place(&mut image);
  }
  if transform.flip_v {
    image::imageops::flip_vertical_in_place(&mut image);
  }
  match transform.quarter_turns % 4 {
    1 => image.rotate90(),
    2 => {
      image::imageops::rotate180_in_place(&mut image);
      image
    },
    3 => image.rotate270(),
    _ => image,
  }
}

/// The pixel size `export` produces for a source of `source` size.
#[must_use]
pub fn output_size(source: (u32, u32), options: &ExportOptions) -> (u32, u32) {
  let width = options.width.max(1);
  let height = options.height.max(1);
  match options.size_rule {
    SizeRule::Exact => (width, height),
    SizeRule::FitInside => {
      let (source_width, source_height) = (source.0.max(1), source.1.max(1));
      let by_width = scale(source_width, source_height, width);
      let by_height = scale(source_height, source_width, height);
      if by_width.0 <= width && by_width.1 <= height {
        by_width
      } else {
        (by_height.1, by_height.0)
      }
    },
  }
}

/// Resize, composite, and encode one image for export.
pub fn export(source: &Decoded, options: &ExportOptions) -> Result<Vec<u8>, Error> {
  let (width, height) = output_size((source.image.width(), source.image.height()), options);
  let resized = if (source.image.width(), source.image.height()) == (width, height) {
    source.image.clone()
  } else {
    source.image.resize_exact(width, height, image::imageops::FilterType::Lanczos3)
  };
  let icc = options.keep_icc.then(|| source.icc.clone()).flatten();
  encode(&resized, options.format, options.background, icc)
}

/// Read clipboard image bytes and return them as PNG with their size. Bytes
/// that already are a PNG are returned unchanged.
pub fn to_png(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), Error> {
  let guessed = image::guess_format(bytes).map_err(|error| pixel_error(&error))?;
  let format = match guessed {
    image::ImageFormat::Png => ImageFormat::Png,
    image::ImageFormat::Jpeg => ImageFormat::Jpeg,
    image::ImageFormat::Gif => ImageFormat::Gif,
    image::ImageFormat::WebP => ImageFormat::WebP,
    image::ImageFormat::Bmp => ImageFormat::Bmp,
    image::ImageFormat::Tiff => ImageFormat::Tiff,
    other => {
      return Err(Error::Format {
        reason: format!("{other:?} is not a supported clipboard image"),
      });
    },
  };
  if format == ImageFormat::Png {
    let (_, width, height) = probe_image(bytes, DocumentKind::Image).map_err(|reason| Error::Format { reason })?;
    return Ok((bytes.to_vec(), width, height));
  }
  let decoded = decode_still(bytes, format)?;
  let (width, height) = (decoded.image.width(), decoded.image.height());
  let png = encode(&decoded.image, OutputFormat::Png, Background::Transparent, None)?;
  Ok((png, width, height))
}
/// Re-encode a document in its own format with `transform` baked in.
pub fn encode_in_place(bytes: &[u8], format: ImageFormat, transform: Transform) -> Result<Vec<u8>, Error> {
  if !format.can_save_in_place() {
    return Err(unsupported(format));
  }
  if format == ImageFormat::Gif {
    let (frames, _) = decode_frames(bytes, format, u64::MAX)?;
    let mut out = Vec::new();
    {
      let mut encoder = image::codecs::gif::GifEncoder::new(&mut out);
      encoder
        .set_repeat(image::codecs::gif::Repeat::Infinite)
        .map_err(|error| pixel_error(&error))?;
      for frame in frames {
        let delay = frame.delay();
        let (left, top) = (frame.left(), frame.top());
        let buffer = transformed(image::DynamicImage::ImageRgba8(frame.into_buffer()), transform).into_rgba8();
        encoder
          .encode_frame(image::Frame::from_parts(buffer, left, top, delay))
          .map_err(|error| pixel_error(&error))?;
      }
    }
    return Ok(out);
  }

  let decoded = decode_still(bytes, format)?;
  let rotated = transformed(decoded.image, transform);
  if format == ImageFormat::Tga {
    let mut out = Vec::new();
    let buffer = rotated.into_rgba8();
    image::codecs::tga::TgaEncoder::new(&mut out)
      .write_image(
        buffer.as_raw(),
        buffer.width(),
        buffer.height(),
        image::ExtendedColorType::Rgba8,
      )
      .map_err(|error| pixel_error(&error))?;
    return Ok(out);
  }
  let output = match format {
    ImageFormat::Jpeg => OutputFormat::Jpeg { quality: 92 },
    ImageFormat::Bmp => OutputFormat::Bmp,
    ImageFormat::Tiff => OutputFormat::Tiff,
    _ => OutputFormat::Png,
  };
  let background = if decoded.has_alpha {
    Background::Transparent
  } else {
    Background::White
  };
  encode(&rotated, output, background, decoded.icc)
}

/// Encode one buffer, compositing when the target cannot store transparency.
fn encode(
  image: &image::DynamicImage,
  format: OutputFormat,
  background: Background,
  icc: Option<Vec<u8>>,
) -> Result<Vec<u8>, Error> {
  let opaque = background != Background::Transparent || !format.supports_alpha();
  let composited = if opaque {
    Some(composite(image, background))
  } else {
    None
  };
  let rgba;
  let rgb;
  let (bytes, color) = if opaque {
    rgb = composited.unwrap_or_else(|| image.to_rgb8());
    (rgb.as_raw().as_slice(), image::ExtendedColorType::Rgb8)
  } else {
    rgba = image.to_rgba8();
    (rgba.as_raw().as_slice(), image::ExtendedColorType::Rgba8)
  };
  let (width, height) = (image.width(), image.height());

  let mut out = Vec::new();
  match format {
    OutputFormat::Png => {
      let mut encoder = image::codecs::png::PngEncoder::new(&mut out);
      set_icc(&mut encoder, icc);
      encoder
        .write_image(bytes, width, height, color)
        .map_err(|error| pixel_error(&error))?;
    },
    OutputFormat::Jpeg { quality } => {
      let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100));
      set_icc(&mut encoder, icc);
      encoder
        .write_image(bytes, width, height, color)
        .map_err(|error| pixel_error(&error))?;
    },
    OutputFormat::WebP => {
      let mut encoder = image::codecs::webp::WebPEncoder::new_lossless(&mut out);
      set_icc(&mut encoder, icc);
      encoder
        .write_image(bytes, width, height, color)
        .map_err(|error| pixel_error(&error))?;
    },
    OutputFormat::Bmp => {
      image::codecs::bmp::BmpEncoder::new(&mut out)
        .write_image(bytes, width, height, color)
        .map_err(|error| pixel_error(&error))?;
    },
    OutputFormat::Tiff => {
      let mut encoder = image::codecs::tiff::TiffEncoder::new(Cursor::new(&mut out));
      set_icc(&mut encoder, icc);
      encoder
        .write_image(bytes, width, height, color)
        .map_err(|error| pixel_error(&error))?;
    },
    OutputFormat::Gif => {
      image::codecs::gif::GifEncoder::new(&mut out)
        .encode(bytes, width, height, color)
        .map_err(|error| pixel_error(&error))?;
    },
  }
  Ok(out)
}

/// Flatten an image onto a solid background.
fn composite(image: &image::DynamicImage, background: Background) -> image::RgbImage {
  let fill = match background {
    Background::Black => 0u8,
    Background::Transparent | Background::White => 255,
  };
  let source = image.to_rgba8();
  let mut out = image::RgbImage::new(source.width(), source.height());
  for (x, y, pixel) in source.enumerate_pixels() {
    let alpha = u32::from(pixel.0[3]);
    let blend = |channel: u8| {
      let over = u32::from(channel) * alpha;
      let under = u32::from(fill) * (255 - alpha);
      u8::try_from((over + under + 127) / 255).unwrap_or(255)
    };
    out.put_pixel(x, y, image::Rgb([blend(pixel.0[0]), blend(pixel.0[1]), blend(pixel.0[2])]));
  }
  out
}

/// Attach a color profile when the encoder supports one; a refusal is not fatal.
fn set_icc<E: image::ImageEncoder>(encoder: &mut E, icc: Option<Vec<u8>>) {
  if let Some(icc) = icc
    && let Err(error) = encoder.set_icc_profile(icc)
  {
    tracing::debug!(%error, "encoder kept no color profile");
  }
}

/// One frame from a still image.
fn still_frame(bytes: &[u8], format: ImageFormat) -> Result<(Vec<image::Frame>, bool), Error> {
  let decoded = decode_still(bytes, format)?;
  Ok((vec![image::Frame::new(decoded.image.into_rgba8())], decoded.has_alpha))
}

/// Collect animation frames within a pixel budget.
fn collect_frames<'a, D: AnimationDecoder<'a>>(decoder: D, max_pixels: u64) -> Result<Vec<image::Frame>, Error> {
  let mut frames = Vec::new();
  let mut pixels: u64 = 0;
  for frame in decoder.into_frames() {
    let frame = frame.map_err(|error| pixel_error(&error))?;
    let (width, height) = frame.buffer().dimensions();
    pixels = pixels.saturating_add(u64::from(width).saturating_mul(u64::from(height)));
    frames.push(frame);
    if pixels >= max_pixels {
      break;
    }
  }
  if frames.is_empty() {
    return Err(Error::Format {
      reason: "the animation has no frames".to_owned(),
    });
  }
  Ok(frames)
}

/// Proportional size for one axis scaled to `target`.
fn scale(primary: u32, secondary: u32, target: u32) -> (u32, u32) {
  let scaled = u64::from(secondary)
    .saturating_mul(u64::from(target))
    .saturating_add(u64::from(primary) / 2)
    / u64::from(primary.max(1));
  (target.max(1), u32::try_from(scaled).unwrap_or(u32::MAX).max(1))
}

fn unsupported(format: ImageFormat) -> Error {
  Error::Format {
    reason: format!("{} images cannot be saved in place; use Export", format.label()),
  }
}

/// One decode or encode failure, without a path: raster works on bytes.
fn pixel_error(error: &image::ImageError) -> Error {
  Error::Format { reason: error.to_string() }
}

#[cfg(test)]
mod tests {
  use std::io::Cursor;

  use super::{
    Background, Decoded, ExportOptions, OutputFormat, SizeRule, Transform, decode_frames, encode_in_place, export,
    output_size, transformed,
  };
  use crate::document::ImageFormat;

  fn png_of(image: &image::RgbaImage) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image.clone())
      .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
      .unwrap();
    bytes
  }

  fn red_blue_png() -> Vec<u8> {
    let mut image = image::RgbaImage::new(2, 1);
    image.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
    image.put_pixel(1, 0, image::Rgba([0, 0, 255, 255]));
    png_of(&image)
  }

  #[test]
  fn transform_composes_and_swaps_size_on_odd_turns() {
    let transform = Transform::IDENTITY.rotate_cw().flip_horizontal();

    assert_eq!(
      transform,
      Transform {
        quarter_turns: 1,
        flip_h: true,
        flip_v: false
      }
    );
    assert_eq!(transform.apply_to_size(30, 20), (20, 30));
    assert_eq!(Transform::IDENTITY.rotate_cw().rotate_ccw(), Transform::IDENTITY);
    assert_eq!(Transform::IDENTITY.rotate_ccw().quarter_turns, 3);
    assert!(Transform::IDENTITY.is_identity());
    assert!(!Transform::IDENTITY.flip_vertical().is_identity());
  }

  #[test]
  fn decode_applies_exif_orientation() {
    let bytes = include_bytes!("../tests/fixtures/orientation6.jpg");

    let decoded = super::decode_still(bytes, ImageFormat::Jpeg).unwrap();

    assert_eq!((decoded.image.width(), decoded.image.height()), (1, 2));
    assert!(!decoded.has_alpha);
  }

  #[test]
  fn decode_reports_alpha_and_frames() {
    let transparent = png_of(&image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 0, 0])));

    let decoded = super::decode_still(&transparent, ImageFormat::Png).unwrap();
    let (frames, has_alpha) = decode_frames(&transparent, ImageFormat::Png, 1024 * 1024).unwrap();

    assert!(decoded.has_alpha);
    assert!(has_alpha);
    assert_eq!(frames.len(), 1);
  }

  #[test]
  fn identity_transform_does_not_copy() {
    let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 4])));
    let ptr = image.as_rgba8().unwrap().as_ptr();
    let out = transformed(image, Transform::IDENTITY);
    assert_eq!(out.as_rgba8().unwrap().as_ptr(), ptr);
  }

  #[test]
  fn encode_in_place_rotates_png_pixels() {
    let out = encode_in_place(&red_blue_png(), ImageFormat::Png, Transform::IDENTITY.rotate_cw()).unwrap();

    let back = image::load_from_memory(&out).unwrap().into_rgba8();
    assert_eq!(back.dimensions(), (1, 2));
    assert_eq!(back.get_pixel(0, 0).0, [255, 0, 0, 255]);
    assert_eq!(back.get_pixel(0, 1).0, [0, 0, 255, 255]);
  }

  #[test]
  fn encode_in_place_refuses_formats_it_cannot_write() {
    let error = encode_in_place(&red_blue_png(), ImageFormat::Ico, Transform::IDENTITY.rotate_cw()).unwrap_err();

    assert!(matches!(error, crate::error::Error::Format { .. }), "got {error:?}");
    assert!(error.to_string().contains("ICO"));
  }

  #[test]
  fn export_fits_inside_and_composites_the_background() {
    let decoded = Decoded {
      image: image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(400, 200, image::Rgba([0, 0, 0, 0]))),
      icc: None,
      has_alpha: true,
    };
    let options = ExportOptions {
      format: OutputFormat::Jpeg { quality: 80 },
      width: 100,
      height: 100,
      size_rule: SizeRule::FitInside,
      background: Background::White,
      keep_icc: false,
    };

    assert_eq!(output_size((400, 200), &options), (100, 50));

    let out = export(&decoded, &options).unwrap();
    let back = image::load_from_memory(&out).unwrap().into_rgb8();
    assert_eq!(back.dimensions(), (100, 50));
    assert_eq!(back.get_pixel(0, 0).0, [255, 255, 255]);
  }

  #[test]
  fn export_exact_ignores_the_aspect_ratio() {
    let options = ExportOptions {
      format: OutputFormat::Png,
      width: 320,
      height: 320,
      size_rule: SizeRule::Exact,
      background: Background::Transparent,
      keep_icc: false,
    };

    assert_eq!(output_size((1920, 1080), &options), (320, 320));
  }

  #[test]
  fn export_keeps_transparency_when_asked() {
    let decoded = Decoded {
      image: image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(4, 4, image::Rgba([10, 20, 30, 0]))),
      icc: None,
      has_alpha: true,
    };
    let options = ExportOptions {
      format: OutputFormat::Png,
      width: 4,
      height: 4,
      size_rule: SizeRule::Exact,
      background: Background::Transparent,
      keep_icc: false,
    };

    let out = export(&decoded, &options).unwrap();

    assert_eq!(image::load_from_memory(&out).unwrap().into_rgba8().get_pixel(0, 0).0[3], 0);
  }

  #[test]
  fn export_strips_the_icc_profile_unless_asked() {
    let decoded = Decoded {
      image: image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(2, 2, image::Rgba([9, 9, 9, 255]))),
      icc: Some(vec![1, 2, 3, 4]),
      has_alpha: false,
    };
    let mut options = ExportOptions {
      format: OutputFormat::Png,
      width: 2,
      height: 2,
      size_rule: SizeRule::Exact,
      background: Background::Transparent,
      keep_icc: false,
    };

    let stripped = export(&decoded, &options).unwrap();
    options.keep_icc = true;
    let kept = export(&decoded, &options).unwrap();

    assert_eq!(icc_of(&stripped), None);
    assert_eq!(icc_of(&kept), Some(vec![1, 2, 3, 4]));
  }

  fn icc_of(png: &[u8]) -> Option<Vec<u8>> {
    use image::ImageDecoder as _;
    image::ImageReader::with_format(Cursor::new(png), image::ImageFormat::Png)
      .into_decoder()
      .unwrap()
      .icc_profile()
      .unwrap()
  }

  #[test]
  fn animated_gif_keeps_every_frame_through_a_rotation() {
    let mut bytes = Vec::new();
    {
      let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
      for color in [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]] {
        let frame = image::Frame::new(image::RgbaImage::from_pixel(4, 2, image::Rgba(color)));
        encoder.encode_frame(frame).unwrap();
      }
    }

    let out = encode_in_place(&bytes, ImageFormat::Gif, Transform::IDENTITY.rotate_cw()).unwrap();

    let (frames, _) = decode_frames(&out, ImageFormat::Gif, 64 * 1024 * 1024).unwrap();
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[0].buffer().dimensions(), (2, 4));
  }
}
