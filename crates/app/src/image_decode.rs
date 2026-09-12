//! Decode images into bounded, GPU-ready frames: the ones a Markdown document
//! embeds, and the ones an image document shows. Both share the 4096 px edge
//! cap and the allocation limits below.

use std::io::Cursor;
use std::sync::Arc;

use gpui_kit::RenderImage;
use image::{AnimationDecoder, Frame, ImageDecoder, RgbaImage};
use openit_core::raster::{Transform, transformed};

/// Maximum bytes read for one image resource.
pub const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

/// Decode allocation ceiling for one image.
const DECODE_ALLOC_LIMIT: u64 = 1536 * 1024 * 1024;

/// Longest edge allowed for a decoded frame.
pub const MAX_IMAGE_EDGE: u32 = 4096;

/// Pixels an animation may keep after downscaling, about 256 MiB of BGRA data.
const MAX_ANIMATION_PIXELS: u64 = 64 * 1024 * 1024;

/// Result of decoding one document image.
pub enum Decoded {
  /// A raster image with one frame for still images or multiple frames for animations.
  Raster(Arc<RenderImage>),
  /// SVG bytes recognized but not rasterized.
  Svg,
  /// Bytes that are not a supported image or could not be decoded.
  Unsupported,
}

/// Sniff and decode supported document image bytes.
pub fn decode(bytes: &[u8]) -> Decoded {
  if is_svg(bytes) {
    return Decoded::Svg;
  }

  let Ok(format) = image::guess_format(bytes) else {
    return Decoded::Unsupported;
  };
  let Some(result) = decode_raster(format, bytes) else {
    return Decoded::Unsupported;
  };
  let Ok(frames) = result else {
    return Decoded::Unsupported;
  };
  if frames.is_empty() {
    return Decoded::Unsupported;
  }

  Decoded::Raster(Arc::new(RenderImage::new(frames)))
}

/// A decoded image document ready to paint.
#[derive(Debug, Clone)]
pub struct DocumentImage {
  /// Frames in GPUI's BGRA order, already transformed and capped.
  pub render: Arc<RenderImage>,
  /// Whether the source carries transparency, which picks the default background.
  pub has_alpha: bool,
}

/// Pack frames GPUI can paint. The buffers must already be BGRA.
pub fn to_render_image(frames: Vec<Frame>) -> Arc<RenderImage> {
  Arc::new(RenderImage::new(frames))
}

/// Decode a raster document at `transform`, off the UI thread.
pub fn decode_document(
  bytes: &[u8],
  format: openit_core::document::ImageFormat,
  transform: Transform,
) -> Result<DocumentImage, String> {
  let (frames, has_alpha) =
    openit_core::raster::decode_frames(bytes, format, MAX_ANIMATION_PIXELS).map_err(|e| e.to_string())?;
  let mut prepared = Vec::with_capacity(frames.len());
  for frame in frames {
    let (left, top, delay) = (frame.left(), frame.top(), frame.delay());
    let buffer = downscaled(frame.into_buffer());
    let buffer = transformed(image::DynamicImage::ImageRgba8(buffer), transform).into_rgba8();
    prepared.push(Frame::from_parts(swap_to_bgra(buffer), left, top, delay));
  }
  if prepared.is_empty() {
    return Err("the image has no frames".to_owned());
  }
  Ok(DocumentImage {
    render: to_render_image(prepared),
    has_alpha,
  })
}

/// Apply `transform` to already-decoded display frames without rereading the file.
pub fn apply_transform(image: &DocumentImage, transform: Transform) -> DocumentImage {
  if transform.is_identity() {
    return image.clone();
  }
  let count = image.render.frame_count();
  let mut frames = Vec::with_capacity(count);
  for index in 0..count {
    let size = image.render.size(index);
    let width = u32::try_from(size.width.0.max(0)).unwrap_or(0);
    let height = u32::try_from(size.height.0.max(0)).unwrap_or(0);
    let Some(bytes) = image.render.as_bytes(index) else {
      continue;
    };
    let Some(buffer) = RgbaImage::from_raw(width, height, bytes.to_vec()) else {
      continue;
    };
    let buffer = transformed(image::DynamicImage::ImageRgba8(buffer), transform).into_rgba8();
    frames.push(Frame::from_parts(buffer, 0, 0, image.render.delay(index)));
  }
  if frames.is_empty() {
    return image.clone();
  }
  DocumentImage {
    render: to_render_image(frames),
    has_alpha: image.has_alpha,
  }
}

fn decode_raster(format: image::ImageFormat, bytes: &[u8]) -> Option<image::ImageResult<Vec<Frame>>> {
  match format {
    image::ImageFormat::Gif => Some(decode_gif(bytes)),
    image::ImageFormat::WebP => Some(decode_webp(bytes)),
    image::ImageFormat::Png
    | image::ImageFormat::Jpeg
    | image::ImageFormat::Bmp
    | image::ImageFormat::Ico
    | image::ImageFormat::Tiff => Some(decode_still(format, bytes)),
    _ => None,
  }
}

fn decode_still(format: image::ImageFormat, bytes: &[u8]) -> image::ImageResult<Vec<Frame>> {
  let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
  reader.limits(limits());
  Ok(vec![Frame::new(fit(reader.decode()?.into_rgba8()))])
}

fn decode_gif(bytes: &[u8]) -> image::ImageResult<Vec<Frame>> {
  let mut decoder = image::codecs::gif::GifDecoder::new(Cursor::new(bytes))?;
  decoder.set_limits(limits())?;
  animated(decoder)
}

fn decode_webp(bytes: &[u8]) -> image::ImageResult<Vec<Frame>> {
  let mut decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(bytes))?;
  decoder.set_limits(limits())?;
  if !within_decode_limit(decoder.dimensions()) {
    return Err(allocation_limit_error());
  }
  if decoder.has_animation() {
    animated(decoder)
  } else {
    Ok(vec![Frame::new(still(decoder)?)])
  }
}

fn still<D: ImageDecoder>(decoder: D) -> image::ImageResult<RgbaImage> {
  Ok(fit(image::DynamicImage::from_decoder(decoder)?.into_rgba8()))
}

/// Downscale each animation frame and stop at the resident pixel budget.
///
/// Once the budget is reached, keep decoded frames and stop reading; a still first frame beats
/// consuming the whole file.
fn animated<'a, D: AnimationDecoder<'a>>(decoder: D) -> image::ImageResult<Vec<Frame>> {
  animated_frames(decoder, MAX_ANIMATION_PIXELS)
}

fn animated_frames<'a, D: AnimationDecoder<'a>>(decoder: D, budget_pixels: u64) -> image::ImageResult<Vec<Frame>> {
  let mut frames = Vec::new();
  let mut pixels = 0_u64;
  for frame in decoder.into_frames() {
    let frame = frame?;
    let (left, top, delay) = (frame.left(), frame.top(), frame.delay());
    let buffer = fit(frame.into_buffer());
    let frame_pixels = u64::from(buffer.width()).saturating_mul(u64::from(buffer.height()));
    let Some(projected_pixels) = pixels.checked_add(frame_pixels) else {
      break;
    };
    if projected_pixels > budget_pixels {
      break;
    }
    pixels = projected_pixels;
    frames.push(Frame::from_parts(buffer, left, top, delay));
    if pixels >= budget_pixels {
      break;
    }
  }
  Ok(frames)
}

/// Downscale to the edge limit, then swap RGBA channels to GPUI's BGRA order.
pub fn fit(buffer: RgbaImage) -> RgbaImage {
  swap_to_bgra(downscaled(buffer))
}

fn swap_to_bgra(mut buffer: RgbaImage) -> RgbaImage {
  for pixel in buffer.pixels_mut() {
    pixel.0.swap(0, 2);
  }
  buffer
}

fn downscaled(buffer: RgbaImage) -> RgbaImage {
  let (width, height) = buffer.dimensions();
  let longest = width.max(height);
  if longest <= MAX_IMAGE_EDGE {
    return buffer;
  }

  let target = (scaled_dimension(width, longest), scaled_dimension(height, longest));
  image::imageops::resize(&buffer, target.0, target.1, image::imageops::FilterType::Triangle)
}

fn scaled_dimension(dimension: u32, longest: u32) -> u32 {
  let numerator = u64::from(dimension).saturating_mul(u64::from(MAX_IMAGE_EDGE));
  let rounded = numerator.saturating_add(u64::from(longest / 2)) / u64::from(longest);
  u32::try_from(rounded).map_or(1, |value| value.max(1))
}

fn limits() -> image::Limits {
  let mut limits = image::Limits::no_limits();
  limits.max_alloc = Some(DECODE_ALLOC_LIMIT);
  limits
}

fn within_decode_limit((width, height): (u32, u32)) -> bool {
  u64::from(width).saturating_mul(u64::from(height)).saturating_mul(4) <= DECODE_ALLOC_LIMIT
}

fn allocation_limit_error() -> image::ImageError {
  image::ImageError::Limits(image::error::LimitError::from_kind(
    image::error::LimitErrorKind::InsufficientMemory,
  ))
}

fn is_svg(bytes: &[u8]) -> bool {
  let Some(first_non_whitespace) = bytes.iter().position(|byte| !byte.is_ascii_whitespace()) else {
    return false;
  };
  let bytes = bytes.get(first_non_whitespace..).unwrap_or_default();
  let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes).trim_ascii_start();
  bytes.starts_with(b"<svg") || bytes.starts_with(b"<?xml")
}

#[cfg(test)]
mod tests {
  use openit_core::raster::Transform;

  use super::{Decoded, MAX_IMAGE_BYTES, MAX_IMAGE_EDGE, animated_frames, decode, decode_document};

  fn png(width: u32, height: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
  }

  fn three_frame_gif() -> Vec<u8> {
    let mut bytes = Vec::new();
    {
      let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
      encoder.set_repeat(image::codecs::gif::Repeat::Infinite).unwrap();
      for [red, green, blue] in [[10_u8, 20, 30], [40, 50, 60], [70, 80, 90]] {
        let buffer = image::RgbaImage::from_pixel(2, 1, image::Rgba([red, green, blue, 255]));
        encoder
          .encode_frame(image::Frame::from_parts(buffer, 0, 0, image::Delay::from_numer_denom_ms(80, 1)))
          .unwrap();
      }
    }
    bytes
  }

  #[test]
  fn decode_document_applies_the_transform_and_reports_alpha() {
    let mut buffer = image::RgbaImage::new(2, 1);
    buffer.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
    buffer.put_pixel(1, 0, image::Rgba([0, 0, 255, 128]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    buffer.write_to(&mut bytes, image::ImageFormat::Png).unwrap();

    let document = decode_document(
      &bytes.into_inner(),
      openit_core::document::ImageFormat::Png,
      Transform::IDENTITY.rotate_cw(),
    )
    .unwrap();

    assert!(document.has_alpha);
    assert_eq!(document.render.frame_count(), 1);
    assert_eq!(
      document.render.size(0),
      gpui_kit::size(gpui_kit::DevicePixels(1), gpui_kit::DevicePixels(2))
    );
    assert_eq!(
      &document.render.as_bytes(0).unwrap()[..4],
      &[0, 0, 255, 255],
      "the red pixel is on top after a clockwise turn, in BGRA"
    );
  }

  #[test]
  fn decode_document_refuses_bytes_it_cannot_read() {
    let error =
      decode_document(b"not an image", openit_core::document::ImageFormat::Png, Transform::IDENTITY).unwrap_err();

    assert!(!error.is_empty());
  }

  #[test]
  fn an_animation_keeps_frames_delays_and_bgra_pixels() {
    let Decoded::Raster(image) = decode(&three_frame_gif()) else {
      panic!("raster expected")
    };
    assert_eq!(image.frame_count(), 3);
    assert_eq!(image.delay(1).numer_denom_ms(), (80, 1));
    let expected = [30_u8, 20, 10, 255, 30, 20, 10, 255];
    assert_eq!(image.as_bytes(0), Some(expected.as_slice()));
  }

  #[test]
  fn a_truncated_gif_is_unsupported() {
    let mut bytes = three_frame_gif();
    bytes.truncate(bytes.len() / 2);
    assert!(matches!(decode(&bytes), Decoded::Unsupported));
  }

  #[test]
  fn animation_stops_before_crossing_and_at_the_pixel_budget() {
    let bytes = three_frame_gif();
    let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(&bytes)).unwrap();
    let frames = animated_frames(decoder, 3).unwrap();
    assert_eq!(frames.len(), 1);
    let pixels = frames
      .iter()
      .map(|frame| {
        let (width, height) = frame.buffer().dimensions();
        u64::from(width) * u64::from(height)
      })
      .sum::<u64>();
    assert!(pixels <= 3);

    let mut truncated = bytes;
    let image_descriptor = [0x2c_u8, 0, 0, 0, 0, 2, 0, 1, 0];
    let markers = truncated
      .windows(image_descriptor.len())
      .enumerate()
      .filter_map(|(index, window)| (window == image_descriptor.as_slice()).then_some(index))
      .collect::<Vec<_>>();
    let third_marker = markers.get(2).copied().unwrap();
    truncated.truncate(third_marker);
    let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(&truncated)).unwrap();
    let frames = animated_frames(decoder, 4).unwrap();
    assert_eq!(frames.len(), 2);
  }

  #[test]
  fn a_small_png_decodes_to_one_bgra_frame() {
    let Decoded::Raster(image) = decode(&png(4, 3)) else {
      panic!("raster expected")
    };
    assert_eq!(
      image.size(0),
      gpui_kit::size(gpui_kit::DevicePixels(4), gpui_kit::DevicePixels(3))
    );
    assert_eq!(image.frame_count(), 1);
    let expected = [30_u8, 20, 10, 255].repeat(4 * 3);
    assert_eq!(image.as_bytes(0), Some(expected.as_slice()));
  }

  #[test]
  fn an_oversized_png_is_downscaled_to_the_edge_limit() {
    let Decoded::Raster(image) = decode(&png(MAX_IMAGE_EDGE * 2, 10)) else {
      panic!("raster expected")
    };
    assert_eq!(
      image.size(0).width,
      gpui_kit::DevicePixels(i32::try_from(MAX_IMAGE_EDGE).unwrap())
    );
  }

  #[test]
  fn svg_is_recognized_and_not_rendered() {
    assert!(matches!(
      decode(b"<svg xmlns='http://www.w3.org/2000/svg'></svg>"),
      Decoded::Svg
    ));
    assert!(matches!(decode(b"\xef\xbb\xbf  <?xml version='1.0'?><svg/>"), Decoded::Svg));
  }

  #[test]
  fn garbage_is_unsupported() {
    assert!(matches!(decode(b"not an image"), Decoded::Unsupported));
    assert!(matches!(decode(&[]), Decoded::Unsupported));
    assert_eq!(MAX_IMAGE_BYTES, 64 * 1024 * 1024);
  }

  #[test]
  fn a_truncated_png_is_unsupported_not_a_panic() {
    let mut bytes = png(64, 64);
    bytes.truncate(bytes.len() / 2);
    assert!(matches!(decode(&bytes), Decoded::Unsupported));
  }
}
