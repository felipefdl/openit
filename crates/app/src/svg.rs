//! Rasterize SVG documents with resvg. The application owns this path so an
//! untrusted document cannot reach files outside its own directory, which is
//! what keeps GPUI's built-in SVG renderer out of the picture.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use gpui_kit::App;
use openit_core::document::MAX_IMAGE_BYTES;
use openit_core::raster::{Transform, transformed};
use resvg::usvg::fontdb;
use resvg::{tiny_skia, usvg};

use crate::image_decode::{DocumentImage, MAX_IMAGE_EDGE, fit, to_render_image};

/// Highest scale a display rasterization uses, so a small SVG still looks
/// crisp when zoomed without decoding an unbounded texture.
const MAX_DISPLAY_SCALE: f32 = 4.0;

/// System fonts, loaded once and shared by every rasterization.
static FONTS: LazyLock<Arc<fontdb::Database>> = LazyLock::new(|| {
  let mut database = fontdb::Database::new();
  database.load_system_fonts();
  Arc::new(database)
});

/// A handle on the shared font database.
fn fonts() -> Arc<fontdb::Database> {
  Arc::clone(&FONTS)
}

/// Start loading system fonts now so the first SVG does not wait for them.
pub(crate) fn warm_fonts(cx: &App) {
  cx.background_executor()
    .spawn(async {
      let _ = fonts();
    })
    .detach();
}

/// Clamp a window or fit scale into the display raster budget.
pub(crate) const fn clamp_display_scale(needed: f32) -> f32 {
  needed.clamp(0.01, MAX_DISPLAY_SCALE)
}

fn display_scale(longest: u32, needed: f32) -> f32 {
  let longest = f32::from(u16::try_from(longest.max(1)).unwrap_or(u16::MAX)).max(1.0);
  let cap = f32::from(u16::try_from(MAX_IMAGE_EDGE).unwrap_or(u16::MAX));
  clamp_display_scale(needed).min(cap / longest)
}

/// The intrinsic pixel size of an SVG document.
pub(crate) fn intrinsic_size(svg: &[u8], base_dir: Option<&Path>) -> Result<(u32, u32), String> {
  let tree = parse(svg, base_dir)?;
  let size = tree.size().to_int_size();
  Ok((size.width(), size.height()))
}

/// Rasterize `svg` at an exact pixel size, stretching each axis as asked.
pub(crate) fn rasterize_to(
  svg: &[u8],
  size: (u32, u32),
  base_dir: Option<&Path>,
) -> Result<(u32, u32, Vec<u8>), String> {
  let tree = parse(svg, base_dir)?;
  let intrinsic = tree.size();
  let (width, height) = (size.0.max(1), size.1.max(1));
  let mut pixmap = tiny_skia::Pixmap::new(width, height).ok_or_else(|| "the image is too large to draw".to_owned())?;
  let scale_x = f32::from(u16::try_from(width).unwrap_or(u16::MAX)) / intrinsic.width().max(1.0);
  let scale_y = f32::from(u16::try_from(height).unwrap_or(u16::MAX)) / intrinsic.height().max(1.0);
  resvg::render(&tree, tiny_skia::Transform::from_scale(scale_x, scale_y), &mut pixmap.as_mut());
  Ok((width, height, into_straight_rgba(pixmap)))
}

/// Rasterize an SVG document for display, transformed and capped like a raster.
pub(crate) fn decode_document(
  bytes: &[u8],
  transform: Transform,
  base_dir: Option<&Path>,
) -> Result<DocumentImage, String> {
  decode_document_at(bytes, transform, base_dir, 1.0)
}

/// Rasterize an SVG at `needed_scale` (window scale factor times fit), capped.
pub(crate) fn decode_document_at(
  bytes: &[u8],
  transform: Transform,
  base_dir: Option<&Path>,
  needed_scale: f32,
) -> Result<DocumentImage, String> {
  let tree = parse(bytes, base_dir)?;
  let intrinsic = tree.size().to_int_size();
  let scale = display_scale(intrinsic.width().max(intrinsic.height()), needed_scale);
  let (width, height, rgba) = rasterize_tree(&tree, scale)?;
  let buffer =
    image::RgbaImage::from_raw(width, height, rgba).ok_or_else(|| "the rasterized image is malformed".to_owned())?;
  let rotated = transformed(image::DynamicImage::ImageRgba8(buffer), transform).into_rgba8();
  Ok(DocumentImage {
    // `fit` also swaps RGBA into the BGRA order GPUI paints.
    render: to_render_image(vec![image::Frame::new(fit(rotated))]),
    has_alpha: true,
  })
}

fn rasterize_tree(tree: &usvg::Tree, scale: f32) -> Result<(u32, u32, Vec<u8>), String> {
  let size = tree
    .size()
    .to_int_size()
    .scale_by(scale)
    .ok_or_else(|| "the image has no drawable size".to_owned())?;
  let mut pixmap =
    tiny_skia::Pixmap::new(size.width(), size.height()).ok_or_else(|| "the image is too large to draw".to_owned())?;
  resvg::render(tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
  Ok((size.width(), size.height(), into_straight_rgba(pixmap)))
}

fn into_straight_rgba(pixmap: tiny_skia::Pixmap) -> Vec<u8> {
  let mut data = pixmap.take();
  for chunk in data.as_chunks_mut::<4>().0 {
    let [r, g, b, a] = chunk;
    let alpha = *a;
    if alpha == 0 || alpha == 255 {
      continue;
    }
    *r = demultiply_channel(*r, alpha);
    *g = demultiply_channel(*g, alpha);
    *b = demultiply_channel(*b, alpha);
  }
  data
}

fn demultiply_channel(channel: u8, alpha: u8) -> u8 {
  if alpha == 0 {
    return 0;
  }
  let n = u32::from(channel) * 255 + u32::from(alpha) / 2;
  u8::try_from(n / u32::from(alpha)).unwrap_or(u8::MAX)
}

/// Parse one SVG document with fonts loaded and local reads bounded.
fn parse(svg: &[u8], base_dir: Option<&Path>) -> Result<usvg::Tree, String> {
  if u64::try_from(svg.len()).unwrap_or(u64::MAX) > MAX_IMAGE_BYTES {
    return Err("the image is too large to draw".to_owned());
  }
  let mut options = usvg::Options {
    resources_dir: base_dir.map(Path::to_path_buf),
    fontdb: fonts(),
    ..usvg::Options::default()
  };
  options.image_href_resolver.resolve_string = bounded_resolver(base_dir);
  usvg::Tree::from_data(svg, &options).map_err(|error| error.to_string())
}

/// usvg's own file resolver, refused for anything outside the document's
/// directory: an untrusted document must not read the rest of the disk.
fn bounded_resolver(base_dir: Option<&Path>) -> usvg::ImageHrefStringResolverFn<'static> {
  let allowed: Option<PathBuf> = base_dir.and_then(|dir| dir.canonicalize().ok());
  let default = usvg::ImageHrefResolver::default_string_resolver();
  Box::new(move |href, options| {
    let allowed = allowed.as_ref()?;
    let path = options.get_abs_path(Path::new(href)).canonicalize().ok()?;
    if !path.starts_with(allowed) {
      tracing::debug!(href, "refused an SVG reference outside the document directory");
      return None;
    }
    default(href, options)
  })
}

#[cfg(test)]
mod tests {
  use openit_core::raster::Transform;

  use std::path::Path;

  fn rasterize(svg: &[u8], scale: f32, base_dir: Option<&Path>) -> Result<(u32, u32, Vec<u8>), String> {
    super::rasterize_tree(&super::parse(svg, base_dir)?, scale)
  }

  #[test]
  fn a_decoded_svg_document_is_in_gpui_channel_order() {
    // A pure orange fill: GPUI paints BGRA, so the blue byte comes first.
    let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><rect width="4" height="4" fill="#e08030"/></svg>"##;

    let document = super::decode_document(svg, Transform::IDENTITY, None).unwrap();

    assert_eq!(&document.render.as_bytes(0).unwrap()[..4], &[0x30, 0x80, 0xe0, 0xff]);
  }

  #[test]
  fn rasterize_scales_the_intrinsic_size() {
    let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="5"><rect width="10" height="5" fill="#ff0000"/></svg>"##;

    let (width, height, rgba) = rasterize(svg, 2.0, None).unwrap();

    assert_eq!((width, height), (20, 10));
    assert_eq!(&rgba[..4], &[255, 0, 0, 255]);
  }

  #[test]
  fn rasterize_keeps_transparency_outside_the_shapes() {
    let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="1"><rect width="1" height="1" fill="#0000ff"/></svg>"##;

    let (_, _, rgba) = rasterize(svg, 1.0, None).unwrap();

    assert_eq!(&rgba[..4], &[0, 0, 255, 255]);
    assert_eq!(rgba[7], 0, "the uncovered pixel stays transparent");
  }

  #[test]
  fn rasterize_ignores_references_outside_the_document_directory() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret.png");
    image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 255, 0, 255]))
      .save(&secret)
      .unwrap();
    let svg = format!(
      r#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"><image href="{}" width="1" height="1"/></svg>"#,
      secret.display()
    );

    let (_, _, rgba) = rasterize(svg.as_bytes(), 1.0, Some(dir.path())).unwrap();

    assert_eq!(rgba[3], 0, "the outside image is not drawn");
  }

  #[test]
  fn rasterize_draws_references_beside_the_document() {
    let dir = tempfile::tempdir().unwrap();
    let neighbour = dir.path().join("dot.png");
    image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 255, 0, 255]))
      .save(&neighbour)
      .unwrap();
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"><image href="dot.png" width="1" height="1"/></svg>"#;

    let (_, _, rgba) = rasterize(svg, 1.0, Some(dir.path())).unwrap();

    assert_eq!(&rgba[..4], &[0, 255, 0, 255]);
  }

  #[test]
  fn rasterize_reports_a_parse_failure() {
    assert!(rasterize(b"<svg", 1.0, None).is_err());
  }
}
