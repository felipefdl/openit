//! PDF to Markdown through `pdf-inspector`, with the extractor's image
//! placeholders replaced by rendered crops of the page.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use pdf_inspector::types::{ItemType, TextItem};
use pdf_inspector::{MarkdownOptions, PdfOptions};

use crate::error::Error;
use crate::pdf::{FIGURE_PAD, FIGURE_SCALE, MAX_FIGURE_EDGE, MIN_FIGURE_EDGE, PageBitmap, PdfDocument, RectPt};

/// One figure lifted out of a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Figure {
  /// Path relative to the Markdown file, always with forward slashes.
  pub relative_path: String,
  /// PNG bytes.
  pub png: Vec<u8>,
}

/// A finished conversion. Nothing is written: the caller decides where the
/// Markdown and its figures go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversion {
  /// The document as Markdown.
  pub markdown: String,
  /// Every figure the Markdown links, in document order.
  pub figures: Vec<Figure>,
  /// One-indexed pages that produced no text.
  pub skipped_pages: Vec<u32>,
}

/// What a conversion produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConvertOutcome {
  /// Markdown, with however many figures the pages held.
  Converted(Conversion),
  /// The document has no text layer to convert.
  NoText,
}

/// The sibling folder that holds a converted document's figures.
#[must_use]
pub fn images_dir_name(stem: &str) -> String {
  format!("{stem}-images")
}

/// Everything one conversion reads.
#[derive(Debug, Clone, Copy)]
pub struct ConvertRequest<'a> {
  /// The parsed document, for rendering figures.
  pub document: &'a PdfDocument,
  /// The document's path, which the extractor needs for an encrypted file.
  pub path: &'a Path,
  /// The document's bytes.
  pub bytes: &'a [u8],
  /// The password the document was opened with, when it has one.
  pub password: Option<&'a str>,
  /// The file name without its extension: names the figure folder.
  pub stem: &'a str,
}

/// Convert a document to Markdown.
///
/// `progress(done, total)` reports finished pages. `cancelled` is checked
/// between pages and before the extractor runs.
///
/// # Errors
///
/// Returns [`Error::Pdf`] when the extractor fails, a figure cannot be
/// rendered, or the conversion was cancelled.
pub fn convert_to_markdown(
  request: &ConvertRequest<'_>,
  progress: &mut dyn FnMut(u32, u32),
  cancelled: &AtomicBool,
) -> Result<ConvertOutcome, Error> {
  let &ConvertRequest { document, path, bytes, password, stem } = request;
  if cancelled.load(Ordering::Relaxed) {
    return Err(cancelled_error());
  }
  let mut options = PdfOptions::new().markdown(markdown_options());
  if let Some(password) = password {
    options = options.password(password);
  }
  let result = pdf_inspector::process_pdf_mem_with_options(bytes, options).map_err(convert_error)?;
  let items = positioned_items(path, bytes, password)?;
  // The classifier withholds Markdown for an image-dominated document, but a
  // page can be mostly artwork and still carry text worth converting, so the
  // extracted runs get the last word.
  let markdown = match result.markdown {
    Some(markdown) if !markdown.trim().is_empty() => markdown,
    _ => pdf_inspector::to_markdown_from_items(items.clone(), markdown_options()),
  };
  let pages = split_pages(&markdown);
  // Figure placeholders are not text: a scanned page is one big image, and a
  // file of nothing but figure links is not a conversion of anything.
  if pages.iter().all(|(_, body)| substitute_figures(body, &[]).trim().is_empty()) {
    return Ok(ConvertOutcome::NoText);
  }

  let images = image_boxes(&items);
  let directory = images_dir_name(stem);
  let mut figures: Vec<Figure> = Vec::new();
  let mut skipped_pages = Vec::new();
  let mut out = String::with_capacity(markdown.len());
  let total = u32::try_from(pages.len()).unwrap_or(u32::MAX);
  for (done, (page, body)) in pages.iter().enumerate() {
    if cancelled.load(Ordering::Relaxed) {
      return Err(cancelled_error());
    }
    let mut links = Vec::new();
    let mut used: HashMap<String, usize> = HashMap::new();
    let mut on_page = 0_usize;
    for name in placeholder_names(body) {
      let candidates = images.get(page).map_or(&[][..], Vec::as_slice);
      let cursor = used.entry(name.clone()).or_insert(0);
      let found = candidates
        .iter()
        .filter(|(candidate, _)| *candidate == name)
        .nth(*cursor)
        .map(|(_, rect)| *rect);
      *cursor = cursor.saturating_add(1);
      let Some(rect) = found else {
        continue;
      };
      if rect.width < MIN_FIGURE_EDGE || rect.height < MIN_FIGURE_EDGE {
        continue;
      }
      let index = usize::try_from(*page).unwrap_or(usize::MAX).saturating_sub(1);
      let geometry = document.pages().get(index).copied().ok_or_else(|| Error::Pdf {
        reason: format!("This PDF has no page {page}"),
      })?;
      let bitmap = crate::pdf::render_region(
        document,
        index,
        geometry.padded(rect, FIGURE_PAD),
        FIGURE_SCALE,
        MAX_FIGURE_EDGE,
      )?;
      on_page = on_page.saturating_add(1);
      let relative_path = format!("{directory}/p{page:03}-{on_page:02}.png");
      figures.push(Figure {
        relative_path: relative_path.clone(),
        png: encode_png(&bitmap)?,
      });
      links.push(relative_path);
    }
    let converted = substitute_figures(body, &links);
    if substitute_figures(body, &[]).trim().is_empty() {
      skipped_pages.push(*page);
    }
    if !converted.trim().is_empty() {
      if !out.is_empty() {
        out.push('\n');
      }
      out.push_str(converted.trim_start_matches('\n'));
    }
    progress(u32::try_from(done).unwrap_or(u32::MAX).saturating_add(1), total);
  }

  Ok(ConvertOutcome::Converted(Conversion { markdown: out, figures, skipped_pages }))
}

/// Image placeholder names and their boxes, keyed by one-indexed page.
type ImagesByPage = HashMap<u32, Vec<(String, RectPt)>>;

/// The Markdown shape every conversion asks for.
fn markdown_options() -> MarkdownOptions {
  MarkdownOptions {
    include_images: true,
    include_page_numbers: true,
    ..MarkdownOptions::default()
  }
}

/// Every positioned run and image box of the document.
fn positioned_items(path: &Path, bytes: &[u8], password: Option<&str>) -> Result<Vec<TextItem>, Error> {
  password
    .map_or_else(
      || pdf_inspector::extract_text_with_positions_mem(bytes),
      |password| pdf_inspector::extract_text_with_positions_pages_with_password(path, None, Some(password)),
    )
    .map_err(convert_error)
}

/// Every image item's placeholder name and box, keyed by one-indexed page.
fn image_boxes(items: &[TextItem]) -> ImagesByPage {
  let mut by_page: ImagesByPage = HashMap::new();
  for item in items {
    if !matches!(item.item_type, ItemType::Image) {
      continue;
    }
    let name = item
      .text
      .strip_prefix("[Image: ")
      .and_then(|rest| rest.strip_suffix(']'))
      .unwrap_or(&item.text)
      .to_owned();
    by_page.entry(item.page).or_default().push((
      name,
      RectPt {
        x: item.x.min(item.x + item.width),
        y: item.y.min(item.y + item.height),
        width: item.width.abs(),
        height: item.height.abs(),
      },
    ));
  }
  by_page
}

/// Split the extractor's output on its page markers. The returned bodies carry
/// no markers: they are scaffolding for this function, not document content.
fn split_pages(markdown: &str) -> Vec<(u32, String)> {
  let mut pages: Vec<(u32, String)> = Vec::new();
  let mut current: Option<(u32, String)> = None;
  for line in markdown.lines() {
    if let Some(page) = page_marker(line) {
      if let Some(previous) = current.take() {
        pages.push(previous);
      }
      current = Some((page, String::new()));
      continue;
    }
    // Content before the first marker belongs to page one.
    let (_, body) = current.get_or_insert_with(|| (1, String::new()));
    body.push_str(line);
    body.push('\n');
  }
  if let Some(last) = current {
    pages.push(last);
  }
  pages
}

/// The page number of a `<!-- Page N -->` marker line.
fn page_marker(line: &str) -> Option<u32> {
  let rest = line.trim().strip_prefix("<!-- Page ")?.strip_suffix("-->")?;
  rest.trim().parse().ok()
}

/// Every `![Image: NAME](image)` placeholder name in a page's Markdown, in
/// order of appearance.
fn placeholder_names(markdown: &str) -> Vec<String> {
  const OPEN: &str = "![Image: ";
  const CLOSE: &str = "](image)";
  let mut names = Vec::new();
  let mut rest = markdown;
  while let Some(start) = rest.find(OPEN) {
    let after = rest.get(start.saturating_add(OPEN.len())..).unwrap_or_default();
    let Some(end) = after.find(CLOSE) else {
      break;
    };
    names.push(after.get(..end).unwrap_or_default().to_owned());
    rest = after.get(end.saturating_add(CLOSE.len())..).unwrap_or_default();
  }
  names
}

/// Replace the placeholders in one page's Markdown with `links`, in order.
/// A placeholder with no link (too small, or no matching item) is dropped.
pub(crate) fn substitute_figures(markdown: &str, links: &[String]) -> String {
  const OPEN: &str = "![Image: ";
  const CLOSE: &str = "](image)";
  let mut out = String::with_capacity(markdown.len());
  let mut rest = markdown;
  let mut next = links.iter();
  while let Some(start) = rest.find(OPEN) {
    out.push_str(rest.get(..start).unwrap_or_default());
    let after = rest.get(start.saturating_add(OPEN.len())..).unwrap_or_default();
    let Some(end) = after.find(CLOSE) else {
      break;
    };
    if let Some(link) = next.next() {
      out.push_str("![Figure](");
      out.push_str(link);
      out.push(')');
    }
    rest = after.get(end.saturating_add(CLOSE.len())..).unwrap_or_default();
  }
  out.push_str(rest);
  out
}

/// Encode a rendered crop as PNG.
fn encode_png(bitmap: &PageBitmap) -> Result<Vec<u8>, Error> {
  let buffer =
    image::RgbaImage::from_raw(bitmap.width, bitmap.height, bitmap.rgba.clone()).ok_or_else(|| Error::Pdf {
      reason: "A figure could not be encoded".to_owned(),
    })?;
  let mut png = Vec::new();
  image::DynamicImage::ImageRgba8(buffer)
    .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
    .map_err(|error| Error::Pdf {
      reason: format!("A figure could not be encoded: {error}"),
    })?;
  Ok(png)
}

fn cancelled_error() -> Error {
  Error::Pdf {
    reason: "Conversion cancelled".to_owned(),
  }
}

fn convert_error(error: pdf_inspector::PdfError) -> Error {
  Error::Pdf {
    reason: match error {
      pdf_inspector::PdfError::Encrypted => "This PDF is password protected".to_owned(),
      pdf_inspector::PdfError::NotAPdf(_) => "This file is not a PDF".to_owned(),
      other => other.to_string(),
    },
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;
  use std::sync::atomic::AtomicBool;

  use super::{
    ConvertOutcome, ConvertRequest, Error, convert_to_markdown, images_dir_name, split_pages, substitute_figures,
  };
  use crate::pdf::test_support::{tiny_pdf_pages, tiny_pdf_with_image};
  use crate::pdf::{PdfDocument, open_pdf};

  fn request<'a>(
    document: &'a PdfDocument,
    path: &'a std::path::Path,
    bytes: &'a [u8],
    password: Option<&'a str>,
    stem: &'a str,
  ) -> ConvertRequest<'a> {
    ConvertRequest { document, path, bytes, password, stem }
  }

  fn write(bytes: &[u8], name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).unwrap();
    (dir, path)
  }

  #[test]
  fn images_dir_is_named_after_the_stem() {
    assert_eq!(images_dir_name("paper"), "paper-images");
  }

  #[test]
  fn placeholders_are_replaced_in_order_and_leftovers_dropped() {
    let markdown = "Intro\n\n![Image: Im0](image)\n\nText\n\n![Image: Im1](image)\n\n![Image: Im2](image)\n";

    let out = substitute_figures(
      markdown,
      &["paper-images/p001-01.png".to_owned(), "paper-images/p001-02.png".to_owned()],
    );

    assert_eq!(
      out,
      "Intro\n\n![Figure](paper-images/p001-01.png)\n\nText\n\n![Figure](paper-images/p001-02.png)\n\n\n"
    );
  }

  #[test]
  fn pages_split_on_the_extractor_markers() {
    let markdown = "lead\n<!-- Page 1 -->\nfirst\n<!-- Page 2 -->\nsecond\n";

    let pages = split_pages(markdown);

    assert_eq!(pages.len(), 3);
    assert_eq!(pages[0], (1, "lead\n".to_owned()));
    assert_eq!(pages[1], (1, "first\n".to_owned()));
    assert_eq!(pages[2], (2, "second\n".to_owned()));
  }

  #[test]
  fn a_text_document_converts_without_figures() {
    let bytes = tiny_pdf_pages(&["Hello World", "Second page"]);
    let (_dir, path) = write(&bytes, "t.pdf");
    let document = open_pdf(Arc::new(bytes.clone()), None).unwrap();
    let mut progress = Vec::new();

    let outcome = convert_to_markdown(
      &request(&document, &path, &bytes, None, "t"),
      &mut |done, total| progress.push((done, total)),
      &AtomicBool::new(false),
    )
    .unwrap();

    let ConvertOutcome::Converted(conversion) = outcome else {
      panic!("a text document converts");
    };
    assert!(conversion.markdown.contains("Hello World"), "{}", conversion.markdown);
    assert!(conversion.markdown.contains("Second page"), "{}", conversion.markdown);
    assert!(!conversion.markdown.contains("<!-- Page"), "{}", conversion.markdown);
    assert!(conversion.figures.is_empty());
    assert_eq!(progress.last(), Some(&(2, 2)));
  }

  #[test]
  fn a_document_without_text_reports_no_text() {
    let bytes = tiny_pdf_with_image("", 0, (612.0, 792.0));
    let (_dir, path) = write(&bytes, "t.pdf");
    let document = open_pdf(Arc::new(bytes.clone()), None).unwrap();

    let outcome = convert_to_markdown(
      &request(&document, &path, &bytes, None, "t"),
      &mut |_, _| {},
      &AtomicBool::new(false),
    )
    .unwrap();

    assert_eq!(outcome, ConvertOutcome::NoText);
  }

  #[test]
  fn an_image_xobject_becomes_a_cropped_png_figure() {
    let bytes = tiny_pdf_with_image("Caption", 0, (612.0, 792.0));
    let (_dir, path) = write(&bytes, "t.pdf");
    let document = open_pdf(Arc::new(bytes.clone()), None).unwrap();

    let outcome = convert_to_markdown(
      &request(&document, &path, &bytes, None, "t"),
      &mut |_, _| {},
      &AtomicBool::new(false),
    )
    .unwrap();

    let ConvertOutcome::Converted(conversion) = outcome else {
      panic!("a text document converts");
    };
    assert_eq!(conversion.figures.len(), 1);
    assert_eq!(conversion.figures[0].relative_path, "t-images/p001-01.png");
    assert!(
      conversion.markdown.contains("![Figure](t-images/p001-01.png)"),
      "{}",
      conversion.markdown
    );
    let decoded = image::load_from_memory(&conversion.figures[0].png).unwrap();
    // 100 x 50 pt of artwork plus 4 pt of slack on every side, at 2 px per pt.
    assert_eq!((decoded.width(), decoded.height()), (216, 116));
  }

  #[test]
  fn a_page_without_text_is_reported_as_skipped() {
    // Two text pages and one page whose only content is an image.
    let mut bytes = tiny_pdf_pages(&["Real text here"]);
    let image_only = tiny_pdf_with_image("", 0, (612.0, 792.0));
    let (_dir, path) = write(&bytes, "t.pdf");
    let document = open_pdf(Arc::new(bytes.clone()), None).unwrap();
    let outcome = convert_to_markdown(
      &request(&document, &path, &bytes, None, "t"),
      &mut |_, _| {},
      &AtomicBool::new(false),
    )
    .unwrap();
    let ConvertOutcome::Converted(conversion) = outcome else {
      panic!("a text page converts");
    };
    assert!(conversion.skipped_pages.is_empty(), "{:?}", conversion.skipped_pages);

    // The image-only document reports no text at all rather than a skip list.
    bytes = image_only;
    let (_dir, path) = write(&bytes, "i.pdf");
    let document = open_pdf(Arc::new(bytes.clone()), None).unwrap();
    let outcome = convert_to_markdown(
      &request(&document, &path, &bytes, None, "i"),
      &mut |_, _| {},
      &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(outcome, ConvertOutcome::NoText);
  }

  #[test]
  fn cancellation_stops_the_conversion() {
    let bytes = tiny_pdf_pages(&["Hello"]);
    let (_dir, path) = write(&bytes, "t.pdf");
    let document = open_pdf(Arc::new(bytes.clone()), None).unwrap();

    let result = convert_to_markdown(
      &request(&document, &path, &bytes, None, "t"),
      &mut |_, _| {},
      &AtomicBool::new(true),
    );

    assert!(matches!(result, Err(Error::Pdf { ref reason }) if reason == "Conversion cancelled"));
  }

  #[test]
  fn an_encrypted_document_converts_with_its_password() {
    let path = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/protected.pdf"));
    let bytes = std::fs::read(&path).unwrap();
    let document = open_pdf(Arc::new(bytes.clone()), Some("openit")).unwrap();

    let outcome = convert_to_markdown(
      &request(&document, &path, &bytes, Some("openit"), "protected"),
      &mut |_, _| {},
      &AtomicBool::new(false),
    )
    .unwrap();

    let ConvertOutcome::Converted(conversion) = outcome else {
      panic!("the password unlocks the text");
    };
    assert!(conversion.markdown.contains("Locked page"), "{}", conversion.markdown);
  }
}
