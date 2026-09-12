//! PDF documents: parse once, report page geometry, and rasterize pages with
//! hayro. Pure CPU, no GPUI, no native dependency.
//!
//! Two coordinate frames meet here. `RectPt` is what `pdf-inspector` reports:
//! points inside the page's visible box, origin at its lower-left corner, `y`
//! growing upward, `/Rotate` not applied. `DisplayRect` is what the reader
//! paints: points in the page as it is shown, origin top-left, `y` growing
//! downward, `/Rotate` applied. [`PageGeometry`] converts between them.

use std::sync::Arc;

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::page::Rotation;
use hayro::hayro_syntax::{DecryptionError, LoadPdfError, Pdf};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{RenderCache, RenderSettings, render};

use crate::error::Error;

/// Longest edge rendered for display, in device pixels. The same texture cap
/// the image viewer uses.
pub const MAX_RENDER_EDGE: u32 = 4096;

/// Device pixels per point used when a figure is cropped out of a page.
pub const FIGURE_SCALE: f32 = 2.0;

/// Longest edge of one cropped figure, in device pixels.
pub const MAX_FIGURE_EDGE: u32 = 2048;

/// Points of slack around a figure's box. An image `XObject`'s box stops at the
/// artwork, so labels drawn beside it as text fall just outside.
pub const FIGURE_PAD: f32 = 4.0;

/// Smallest figure worth cropping, in points.
pub const MIN_FIGURE_EDGE: f32 = 8.0;

/// A rectangle in the item frame: points inside the page's visible box, origin
/// at its lower-left corner, `y` growing upward, before `/Rotate`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RectPt {
  /// Distance from the left edge of the visible box.
  pub x: f32,
  /// Distance from the bottom edge of the visible box.
  pub y: f32,
  /// Width in points.
  pub width: f32,
  /// Height in points.
  pub height: f32,
}

/// A rectangle in the displayed page: points with the origin at the top-left
/// corner, `y` growing downward, after `/Rotate`. Multiply by the render scale
/// for pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayRect {
  /// Distance from the left edge of the displayed page.
  pub x: f32,
  /// Distance from the top edge of the displayed page.
  pub y: f32,
  /// Width in points.
  pub width: f32,
  /// Height in points.
  pub height: f32,
}

/// What is known about a page before it renders.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageGeometry {
  /// Visible box width in points, before rotation.
  pub width: f32,
  /// Visible box height in points, before rotation.
  pub height: f32,
  /// `/Rotate`, normalized to 0, 90, 180, or 270.
  pub rotation: u16,
}

impl PageGeometry {
  /// Displayed size in points: width and height swap for 90 and 270. Matches
  /// hayro's own `render_dimensions`.
  #[must_use]
  pub const fn display_size(self) -> (f32, f32) {
    match self.rotation {
      90 | 270 => (self.height, self.width),
      _ => (self.width, self.height),
    }
  }

  /// Map an item box into the displayed page.
  #[must_use]
  pub fn to_display(self, rect: RectPt) -> DisplayRect {
    let (x, y, w, h) = (rect.x, self.height - rect.y - rect.height, rect.width, rect.height);
    match self.rotation {
      90 => DisplayRect {
        x: self.height - y - h,
        y: x,
        width: h,
        height: w,
      },
      180 => DisplayRect {
        x: self.width - x - w,
        y: self.height - y - h,
        width: w,
        height: h,
      },
      270 => DisplayRect {
        x: y,
        y: self.width - x - w,
        width: h,
        height: w,
      },
      _ => DisplayRect { x, y, width: w, height: h },
    }
  }

  /// Map a point in the displayed page back into the item frame.
  #[must_use]
  pub fn to_page(self, x: f32, y: f32) -> (f32, f32) {
    let (unrotated_x, unrotated_y) = match self.rotation {
      90 => (y, self.height - x),
      180 => (self.width - x, self.height - y),
      270 => (self.width - y, x),
      _ => (x, y),
    };
    (unrotated_x, self.height - unrotated_y)
  }

  /// Grow a box by `pad` points on every side, clamped to the visible box.
  #[must_use]
  pub fn padded(self, rect: RectPt, pad: f32) -> RectPt {
    let x = (rect.x - pad).max(0.0);
    let y = (rect.y - pad).max(0.0);
    let right = (rect.x + rect.width + pad).min(self.width);
    let top = (rect.y + rect.height + pad).min(self.height);
    RectPt {
      x,
      y,
      width: (right - x).max(0.0),
      height: (top - y).max(0.0),
    }
  }
}

/// Why a PDF could not be opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PdfOpenError {
  /// The document is encrypted and no password was given.
  #[error("This PDF is password protected")]
  NeedsPassword,
  /// The password did not unlock the document.
  #[error("That password did not unlock the document")]
  WrongPassword,
  /// The document uses encryption the reader cannot handle.
  #[error("This PDF uses an encryption method OpenIt cannot read")]
  UnsupportedEncryption,
  /// The document could not be parsed.
  #[error("This PDF is malformed")]
  Malformed,
}

/// A parsed document. `Send + Sync`: share it behind an `Arc`.
pub struct PdfDocument {
  pdf: Pdf,
  pages: Vec<PageGeometry>,
}

impl std::fmt::Debug for PdfDocument {
  /// Never the document itself: it holds every byte of the file.
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("PdfDocument")
      .field("pages", &self.pages.len())
      .finish_non_exhaustive()
  }
}

impl PdfDocument {
  /// How many pages the document has.
  #[must_use]
  pub const fn page_count(&self) -> usize {
    self.pages.len()
  }

  /// Every page's geometry, in document order.
  #[must_use]
  pub fn pages(&self) -> &[PageGeometry] {
    &self.pages
  }
}

/// Parse `bytes`. `None` tries the empty user password, which is what an
/// unencrypted document and an owner-password-only document both accept.
///
/// # Errors
///
/// Returns [`PdfOpenError`] when the document is encrypted or malformed.
pub fn open_pdf(bytes: Arc<Vec<u8>>, password: Option<&str>) -> Result<PdfDocument, PdfOpenError> {
  // hayro reports a missing and a wrong password the same way, so the caller's
  // own input decides which one this is.
  let pdf = Pdf::new_with_password(bytes, password.unwrap_or_default()).map_err(|error| match error {
    LoadPdfError::Decryption(DecryptionError::PasswordProtected) if password.is_none() => PdfOpenError::NeedsPassword,
    LoadPdfError::Decryption(DecryptionError::PasswordProtected) => PdfOpenError::WrongPassword,
    LoadPdfError::Decryption(DecryptionError::UnsupportedAlgorithm) => PdfOpenError::UnsupportedEncryption,
    LoadPdfError::Decryption(_) | LoadPdfError::Invalid => PdfOpenError::Malformed,
  })?;
  let pages = pdf
    .pages()
    .iter()
    .map(|page| {
      let (width, height) = page.base_dimensions();
      PageGeometry {
        width,
        height,
        rotation: rotation_degrees(page.rotation()),
      }
    })
    .collect();
  Ok(PdfDocument { pdf, pages })
}

/// One rasterized page or region: row-major straight-alpha RGBA, opaque
/// because the page is composited over white paper.
pub struct PageBitmap {
  /// Zero-based page index the pixels came from.
  pub page: usize,
  /// Width in device pixels.
  pub width: u32,
  /// Height in device pixels.
  pub height: u32,
  /// Row-major RGBA8.
  pub rgba: Vec<u8>,
}

impl std::fmt::Debug for PageBitmap {
  /// Never the pixels: a page is megabytes of them.
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("PageBitmap")
      .field("page", &self.page)
      .field("width", &self.width)
      .field("height", &self.height)
      .finish_non_exhaustive()
  }
}

/// Rasterize a whole page at `scale` device pixels per point, capped at
/// [`MAX_RENDER_EDGE`] on the longest edge.
///
/// # Errors
///
/// Returns [`Error::Pdf`] when the page does not exist or renders empty.
pub fn render_page(document: &PdfDocument, page: usize, scale: f32) -> Result<PageBitmap, Error> {
  let geometry = *document.pages.get(page).ok_or_else(|| missing_page(page))?;
  let (display_width, display_height) = geometry.display_size();
  let scale = capped_scale(scale, display_width.max(display_height), MAX_RENDER_EDGE);
  rasterize(document, page, scale, None)
}

/// Rasterize the part of `page` under `region` (item frame) at `scale`, capped
/// at `max_edge` on the longest edge of the crop.
///
/// # Errors
///
/// Returns [`Error::Pdf`] when the page does not exist, the region is empty, or
/// the page renders empty.
pub fn render_region(
  document: &PdfDocument,
  page: usize,
  region: RectPt,
  scale: f32,
  max_edge: u32,
) -> Result<PageBitmap, Error> {
  let geometry = *document.pages.get(page).ok_or_else(|| missing_page(page))?;
  let shown = geometry.to_display(region);
  if shown.width <= 0.0 || shown.height <= 0.0 {
    return Err(Error::Pdf {
      reason: format!("Page {} has no artwork at that position", page.saturating_add(1)),
    });
  }
  let scale = capped_scale(scale, shown.width.max(shown.height), max_edge);
  rasterize(document, page, scale, Some(shown))
}

/// The scale that keeps `longest_edge_pt * scale` at or under `max_edge`.
fn capped_scale(scale: f32, longest_edge_pt: f32, max_edge: u32) -> f32 {
  let requested = if scale.is_finite() && scale > 0.0 {
    scale
  } else {
    1.0
  };
  let longest = longest_edge_pt.max(1.0);
  let cap = f32::from(u16::try_from(max_edge).unwrap_or(u16::MAX)) / longest;
  requested.min(cap).max(f32::MIN_POSITIVE)
}

/// Render `page` at `scale`, optionally keeping only the pixels under `crop`.
fn rasterize(document: &PdfDocument, page: usize, scale: f32, crop: Option<DisplayRect>) -> Result<PageBitmap, Error> {
  let target = document.pdf.pages().get(page).ok_or_else(|| missing_page(page))?;
  let settings = RenderSettings {
    x_scale: scale,
    y_scale: scale,
    bg_color: WHITE,
    ..RenderSettings::default()
  };
  // `RenderCache` is neither `Send` nor `Sync` and borrows the document, so it
  // cannot outlive one render call. Reusing one across pages measured no
  // faster, so each call gets its own.
  let pixmap = render(target, &RenderCache::new(), &InterpreterSettings::default(), &settings);
  let (width, height) = (u32::from(pixmap.width()), u32::from(pixmap.height()));
  if width == 0 || height == 0 {
    return Err(Error::Pdf {
      reason: format!("Page {} rendered empty", page.saturating_add(1)),
    });
  }
  // The page is composited over opaque white, so premultiplied and straight
  // alpha agree and the bytes need no per-pixel conversion.
  let rgba = pixmap.data_as_u8_slice();
  let Some(crop) = crop else {
    return Ok(PageBitmap { page, width, height, rgba: rgba.to_vec() });
  };
  crop_rgba(page, rgba, width, height, crop, scale)
}

/// Copy the pixels under `crop` out of a full-page bitmap.
fn crop_rgba(
  page: usize,
  rgba: &[u8],
  width: u32,
  height: u32,
  crop: DisplayRect,
  scale: f32,
) -> Result<PageBitmap, Error> {
  let left = round_to_u32(crop.x * scale).min(width);
  let top = round_to_u32(crop.y * scale).min(height);
  let crop_width = round_to_u32(crop.width * scale).min(width.saturating_sub(left));
  let crop_height = round_to_u32(crop.height * scale).min(height.saturating_sub(top));
  if crop_width == 0 || crop_height == 0 {
    return Err(Error::Pdf {
      reason: format!("Page {} has no artwork at that position", page.saturating_add(1)),
    });
  }
  let row_bytes = usize::try_from(width).unwrap_or(usize::MAX).saturating_mul(4);
  let crop_row_bytes = usize::try_from(crop_width).unwrap_or(usize::MAX).saturating_mul(4);
  let left_bytes = usize::try_from(left).unwrap_or(usize::MAX).saturating_mul(4);
  let mut out = Vec::with_capacity(crop_row_bytes.saturating_mul(usize::try_from(crop_height).unwrap_or(0)));
  for row in top..top.saturating_add(crop_height) {
    let start = usize::try_from(row)
      .unwrap_or(usize::MAX)
      .saturating_mul(row_bytes)
      .saturating_add(left_bytes);
    let end = start.saturating_add(crop_row_bytes);
    let slice = rgba.get(start..end).ok_or_else(|| Error::Pdf {
      reason: format!("Page {} could not be cropped", page.saturating_add(1)),
    })?;
    out.extend_from_slice(slice);
  }
  Ok(PageBitmap {
    page,
    width: crop_width,
    height: crop_height,
    rgba: out,
  })
}

fn missing_page(page: usize) -> Error {
  Error::Pdf {
    reason: format!("This PDF has no page {}", page.saturating_add(1)),
  }
}

const fn rotation_degrees(rotation: Rotation) -> u16 {
  match rotation {
    Rotation::None => 0,
    Rotation::Horizontal => 90,
    Rotation::Flipped => 180,
    Rotation::FlippedHorizontal => 270,
  }
}

/// Round a finite, non-negative `f32` to the nearest `u32`, saturating.
#[expect(
  clippy::as_conversions,
  clippy::cast_possible_truncation,
  clippy::cast_sign_loss,
  reason = "no checked float-to-integer conversion exists; the value is clamped into range first"
)]
fn round_to_u32(value: f32) -> u32 {
  if !value.is_finite() || value <= 0.0 {
    return 0;
  }
  value.round().min(f32::from(u16::MAX)) as u32
}

#[cfg(any(test, feature = "fixtures"))]
pub mod test_support {
  //! Hand-written PDFs for tests: no generator library, no fixture files.

  /// Wrap `objects` in a PDF file with a cross-reference table.
  fn assemble(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, object) in objects.iter().enumerate() {
      offsets.push(out.len());
      out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
      out.extend_from_slice(object);
      out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets {
      out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
      format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
      )
      .as_bytes(),
    );
    out
  }

  fn stream(dictionary: &str, content: &[u8]) -> Vec<u8> {
    let mut object = format!("<< {dictionary} /Length {} >>\nstream\n", content.len()).into_bytes();
    object.extend_from_slice(content);
    object.extend_from_slice(b"\nendstream");
    object
  }

  /// One page with a line of Helvetica text, an optional `/Rotate`, a media
  /// box, and an optional crop box.
  #[must_use]
  pub fn tiny_pdf(text: &str, rotate: u16, media: (f32, f32), crop: Option<(f32, f32, f32, f32)>) -> Vec<u8> {
    let content = format!("BT /F1 24 Tf 72 {} Td ({text}) Tj ET", media.1 - 100.0);
    let crop = crop.map_or(String::new(), |(x0, y0, x1, y1)| format!(" /CropBox [{x0} {y0} {x1} {y1}]"));
    let objects = [
      b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
      b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
      format!(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}]{crop} /Rotate {rotate} \
         /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>",
        media.0, media.1
      )
      .into_bytes(),
      b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
      stream("", content.as_bytes()),
    ];
    assemble(&objects)
  }

  /// One Letter page per entry, each with one line of Helvetica text.
  #[must_use]
  pub fn tiny_pdf_pages(texts: &[&str]) -> Vec<u8> {
    let kids: Vec<String> = (0..texts.len()).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
    let mut objects = vec![
      b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
      format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), texts.len()).into_bytes(),
      b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
    ];
    for (index, text) in texts.iter().enumerate() {
      let content = format!("BT /F1 24 Tf 72 692 Td ({text}) Tj ET");
      objects.push(
        format!(
          "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
           /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
          5 + 2 * index
        )
        .into_bytes(),
      );
      objects.push(stream("", content.as_bytes()));
    }
    assemble(&objects)
  }

  /// One page with a line of text and a 2x2 RGB image drawn 100 x 50 pt at
  /// (72, 500).
  #[must_use]
  pub fn tiny_pdf_with_image(text: &str, rotate: u16, media: (f32, f32)) -> Vec<u8> {
    let pixels: [u8; 12] = [255, 0, 0, 0, 255, 0, 0, 0, 255, 128, 128, 128];
    let content = format!(
      "BT /F1 24 Tf 72 {} Td ({text}) Tj ET\nq 100 0 0 50 72 500 cm /Im0 Do Q",
      media.1 - 100.0
    );
    let objects = [
      b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
      b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
      format!(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Rotate {rotate} \
         /Resources << /Font << /F1 4 0 R >> /XObject << /Im0 6 0 R >> >> /Contents 5 0 R >>",
        media.0, media.1
      )
      .into_bytes(),
      b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
      stream("", content.as_bytes()),
      stream(
        "/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8",
        &pixels,
      ),
    ];
    assemble(&objects)
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use super::test_support::{tiny_pdf, tiny_pdf_pages, tiny_pdf_with_image};
  use super::{
    Error, MAX_FIGURE_EDGE, MAX_RENDER_EDGE, PageGeometry, PdfOpenError, RectPt, open_pdf, render_page, render_region,
  };

  #[test]
  fn open_reports_page_geometry_from_the_visible_box() {
    let document = open_pdf(
      Arc::new(tiny_pdf("Hello", 0, (612.0, 792.0), Some((50.0, 50.0, 350.0, 450.0)))),
      None,
    )
    .unwrap();

    assert_eq!(document.page_count(), 1);
    let page = document.pages()[0];
    assert_eq!((page.width, page.height, page.rotation), (300.0, 400.0, 0));
    assert_eq!(page.display_size(), (300.0, 400.0));
  }

  #[test]
  fn open_reads_every_page() {
    let document = open_pdf(Arc::new(tiny_pdf_pages(&["one", "two", "three"])), None).unwrap();

    assert_eq!(document.page_count(), 3);
    assert!(document.pages().iter().all(|page| page.display_size() == (612.0, 792.0)));
  }

  #[test]
  fn rotation_swaps_the_display_size_and_maps_boxes() {
    let page = PageGeometry {
      width: 300.0,
      height: 400.0,
      rotation: 90,
    };
    assert_eq!(page.display_size(), (400.0, 300.0));

    // The lower-left corner of the unrotated page is the top-left corner after
    // a clockwise quarter turn.
    let shown = page.to_display(RectPt {
      x: 0.0,
      y: 0.0,
      width: 10.0,
      height: 10.0,
    });
    assert!(shown.x.abs() < 1e-3 && shown.y.abs() < 1e-3, "{shown:?}");

    let upright = PageGeometry { width: 300.0, height: 400.0, rotation: 0 };
    let shown = upright.to_display(RectPt {
      x: 10.0,
      y: 20.0,
      width: 30.0,
      height: 40.0,
    });
    assert_eq!((shown.x, shown.y, shown.width, shown.height), (10.0, 340.0, 30.0, 40.0));
  }

  #[test]
  fn every_rotation_maps_inside_the_page_and_back() {
    let rect = RectPt {
      x: 10.0,
      y: 20.0,
      width: 30.0,
      height: 40.0,
    };
    for rotation in [0, 90, 180, 270] {
      let page = PageGeometry { width: 300.0, height: 400.0, rotation };
      let shown = page.to_display(rect);
      let (display_width, display_height) = page.display_size();
      assert!(
        shown.x >= -1e-3
          && shown.y >= -1e-3
          && shown.x + shown.width <= display_width + 1e-3
          && shown.y + shown.height <= display_height + 1e-3,
        "rotation {rotation}: {shown:?} outside {display_width}x{display_height}"
      );
      // The display top-left corner is a different item corner per rotation.
      let expected = match rotation {
        0 => (rect.x, rect.y + rect.height),
        90 => (rect.x, rect.y),
        180 => (rect.x + rect.width, rect.y),
        _ => (rect.x + rect.width, rect.y + rect.height),
      };
      let (back_x, back_y) = page.to_page(shown.x, shown.y);
      assert!(
        (back_x - expected.0).abs() < 1e-3 && (back_y - expected.1).abs() < 1e-3,
        "rotation {rotation}: ({back_x}, {back_y}) is not {expected:?}"
      );
    }
  }

  #[test]
  fn padding_a_box_stops_at_the_page_edges() {
    let page = PageGeometry { width: 100.0, height: 100.0, rotation: 0 };
    let padded = page.padded(
      RectPt {
        x: 2.0,
        y: 96.0,
        width: 10.0,
        height: 3.0,
      },
      4.0,
    );
    assert_eq!((padded.x, padded.y), (0.0, 92.0));
    assert_eq!((padded.width, padded.height), (16.0, 8.0));
  }

  #[test]
  fn render_page_produces_an_opaque_bitmap_of_the_expected_size() {
    let document = open_pdf(Arc::new(tiny_pdf("Hello", 0, (200.0, 100.0), None)), None).unwrap();

    let bitmap = render_page(&document, 0, 2.0).unwrap();

    assert_eq!((bitmap.width, bitmap.height), (400, 200));
    assert_eq!(bitmap.rgba.len(), 400 * 200 * 4);
    assert!(bitmap.rgba.chunks(4).all(|pixel| pixel[3] == 255));
    assert!(
      bitmap.rgba.chunks(4).any(|pixel| pixel[0] < 128),
      "the text should paint dark pixels"
    );
  }

  #[test]
  fn render_page_caps_the_longest_edge() {
    let document = open_pdf(Arc::new(tiny_pdf("x", 0, (2000.0, 1000.0), None)), None).unwrap();

    let bitmap = render_page(&document, 0, 4.0).unwrap();

    assert_eq!(bitmap.width, MAX_RENDER_EDGE);
    assert_eq!(bitmap.height, MAX_RENDER_EDGE / 2);
  }

  #[test]
  fn render_page_refuses_a_page_that_does_not_exist() {
    let document = open_pdf(Arc::new(tiny_pdf("x", 0, (200.0, 100.0), None)), None).unwrap();

    assert!(matches!(render_page(&document, 4, 1.0), Err(Error::Pdf { .. })));
  }

  #[test]
  fn render_region_crops_to_the_requested_box() {
    let document = open_pdf(Arc::new(tiny_pdf_with_image("Caption", 0, (612.0, 792.0))), None).unwrap();

    let region = RectPt {
      x: 72.0,
      y: 500.0,
      width: 100.0,
      height: 50.0,
    };
    let bitmap = render_region(&document, 0, region, 2.0, MAX_FIGURE_EDGE).unwrap();

    assert_eq!((bitmap.width, bitmap.height), (200, 100));
    // The image is drawn there, so the crop cannot be blank white paper.
    assert!(
      bitmap
        .rgba
        .chunks(4)
        .any(|pixel| pixel[0] != 255 || pixel[1] != 255 || pixel[2] != 255),
      "the figure's pixels should be in the crop"
    );
  }

  #[test]
  fn render_region_caps_the_crop_edge() {
    let document = open_pdf(Arc::new(tiny_pdf("x", 0, (2000.0, 1000.0), None)), None).unwrap();

    let region = RectPt {
      x: 0.0,
      y: 0.0,
      width: 2000.0,
      height: 1000.0,
    };
    let bitmap = render_region(&document, 0, region, 8.0, 512).unwrap();

    assert_eq!((bitmap.width, bitmap.height), (512, 256));
  }

  #[test]
  fn malformed_bytes_are_reported_as_malformed() {
    assert!(matches!(
      open_pdf(Arc::new(b"%PDF-1.4 garbage".to_vec()), None),
      Err(PdfOpenError::Malformed)
    ));
  }

  #[test]
  fn a_protected_file_asks_for_its_password() {
    let bytes = Arc::new(std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/protected.pdf")).unwrap());

    assert!(matches!(open_pdf(Arc::clone(&bytes), None), Err(PdfOpenError::NeedsPassword)));
    assert!(matches!(
      open_pdf(Arc::clone(&bytes), Some("nope")),
      Err(PdfOpenError::WrongPassword)
    ));
    assert_eq!(open_pdf(bytes, Some("openit")).map(|doc| doc.page_count()).ok(), Some(1));
  }
}
