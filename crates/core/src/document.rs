//! Loading documents and tagging buffer snapshots with a revision.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

use image::ImageDecoder as _;
use ropey::Rope;

use crate::error::Error;
use crate::kind::{DocumentKind, detect};
use crate::watch::Fingerprint;

/// Largest file the text reader accepts. The cap is enforced on the bytes
/// actually read from one open file handle, so growth after metadata cannot bypass it.
pub const MAX_TEXT_BYTES: u64 = 5 * 1024 * 1024;

/// Largest file the image reader accepts, enforced the same way.
pub const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

/// Largest PDF the reader accepts, enforced the same way.
pub const MAX_PDF_BYTES: u64 = 256 * 1024 * 1024;

/// Allocation ceiling for one header probe or decode.
pub const DECODE_ALLOC_LIMIT: u64 = 1536 * 1024 * 1024;

/// Monotonic counter for one document session's buffer. Every change bumps it;
/// every background result carries the revision it started from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Revision(u64);

impl Revision {
  /// Revision of freshly loaded text.
  pub const INITIAL: Self = Self(0);

  /// The revision after one more change.
  #[must_use]
  pub const fn next(self) -> Self {
    Self(self.0.saturating_add(1))
  }
}

/// A point-in-time view of the buffer. `Rope::clone` is O(1) and shares
/// structure with the live editor buffer, so taking one per save or
/// validation is cheap.
#[derive(Debug, Clone)]
pub struct Snapshot {
  /// Revision the text belongs to.
  pub revision: Revision,
  /// The text at that revision.
  pub text: Rope,
}

/// Text read from disk with its detected kind and source fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
  /// How to present it.
  pub kind: DocumentKind,
  /// File contents.
  pub text: String,
  /// Source file fingerprint captured from the open file handle.
  pub disk: Fingerprint,
}

/// Read a text document. Refuses unsupported kinds, files over
/// [`MAX_TEXT_BYTES`], and non-UTF-8 content.
pub fn load_text(path: &Path) -> Result<Loaded, Error> {
  let path_buf = path.to_path_buf();
  let path_metadata = fs::metadata(path).map_err(|source| Error::Read { path: path_buf.clone(), source })?;
  if !path_metadata.is_file() {
    return Err(Error::Read {
      path: path_buf,
      source: io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"),
    });
  }
  let file = File::open(path).map_err(|source| Error::Read { path: path_buf.clone(), source })?;
  let metadata = file
    .metadata()
    .map_err(|source| Error::Read { path: path_buf.clone(), source })?;
  if !metadata.is_file() {
    return Err(Error::Read {
      path: path_buf,
      source: io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"),
    });
  }
  let disk = Fingerprint::of_file(&file).map_err(|source| Error::Read { path: path_buf.clone(), source })?;
  let kind = detect(path);
  if !matches!(kind, DocumentKind::Markdown | DocumentKind::Text { .. }) {
    return Err(Error::Unsupported { path: path.to_path_buf() });
  }
  let size = metadata.len();
  if size > MAX_TEXT_BYTES {
    return Err(Error::TooLarge {
      path: path.to_path_buf(),
      size,
      limit: MAX_TEXT_BYTES,
    });
  }
  let capacity = usize::try_from(size.min(MAX_TEXT_BYTES)).unwrap_or(usize::MAX);
  let mut bytes = Vec::with_capacity(capacity);
  let bytes_read = file
    .take(MAX_TEXT_BYTES + 1)
    .read_to_end(&mut bytes)
    .map_err(|source| Error::Read { path: path.to_path_buf(), source })?;
  let bytes_read = u64::try_from(bytes_read).unwrap_or(u64::MAX);
  if bytes_read > MAX_TEXT_BYTES {
    return Err(Error::TooLarge {
      path: path.to_path_buf(),
      size: bytes_read,
      limit: MAX_TEXT_BYTES,
    });
  }
  let text = String::from_utf8(bytes).map_err(|_| Error::NotUtf8 { path: path.to_path_buf() })?;
  Ok(Loaded { kind, text, disk })
}

/// An image format OpenIt reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageFormat {
  /// PNG, including APNG animations.
  Png,
  /// JPEG.
  Jpeg,
  /// GIF, including animations.
  Gif,
  /// WebP, including animations.
  WebP,
  /// Windows bitmap.
  Bmp,
  /// Windows icon or cursor.
  Ico,
  /// TIFF.
  Tiff,
  /// Truevision TGA.
  Tga,
  /// Netpbm (PBM, PGM, PPM, PAM).
  Pnm,
  /// SVG, rasterized by the application.
  Svg,
}

impl ImageFormat {
  /// Upper-case label for the status bar.
  pub const fn label(self) -> &'static str {
    match self {
      Self::Png => "PNG",
      Self::Jpeg => "JPEG",
      Self::Gif => "GIF",
      Self::WebP => "WEBP",
      Self::Bmp => "BMP",
      Self::Ico => "ICO",
      Self::Tiff => "TIFF",
      Self::Tga => "TGA",
      Self::Pnm => "PNM",
      Self::Svg => "SVG",
    }
  }

  /// The `image` crate format, or `None` for SVG.
  pub const fn raster(self) -> Option<image::ImageFormat> {
    match self {
      Self::Png => Some(image::ImageFormat::Png),
      Self::Jpeg => Some(image::ImageFormat::Jpeg),
      Self::Gif => Some(image::ImageFormat::Gif),
      Self::WebP => Some(image::ImageFormat::WebP),
      Self::Bmp => Some(image::ImageFormat::Bmp),
      Self::Ico => Some(image::ImageFormat::Ico),
      Self::Tiff => Some(image::ImageFormat::Tiff),
      Self::Tga => Some(image::ImageFormat::Tga),
      Self::Pnm => Some(image::ImageFormat::Pnm),
      Self::Svg => None,
    }
  }

  /// Whether a save can rewrite the document in this format.
  pub const fn can_save_in_place(self) -> bool {
    matches!(self, Self::Png | Self::Jpeg | Self::Gif | Self::Bmp | Self::Tiff | Self::Tga)
  }

  const fn from_raster(format: image::ImageFormat) -> Option<Self> {
    match format {
      image::ImageFormat::Png => Some(Self::Png),
      image::ImageFormat::Jpeg => Some(Self::Jpeg),
      image::ImageFormat::Gif => Some(Self::Gif),
      image::ImageFormat::WebP => Some(Self::WebP),
      image::ImageFormat::Bmp => Some(Self::Bmp),
      image::ImageFormat::Ico => Some(Self::Ico),
      image::ImageFormat::Tiff => Some(Self::Tiff),
      image::ImageFormat::Tga => Some(Self::Tga),
      image::ImageFormat::Pnm => Some(Self::Pnm),
      _ => None,
    }
  }
}

/// An image document read from disk: the original bytes plus header facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedImage {
  /// How to present it.
  pub kind: DocumentKind,
  /// The format the bytes are in.
  pub format: ImageFormat,
  /// The file contents, unmodified.
  pub bytes: Vec<u8>,
  /// Width from the header, before any transform.
  pub width: u32,
  /// Height from the header, before any transform.
  pub height: u32,
  /// Source file fingerprint captured from the open file handle.
  pub disk: Fingerprint,
}

/// Read an image document: bounded bytes plus header dimensions, no pixel decode.
pub fn load_image(path: &Path) -> Result<LoadedImage, Error> {
  let kind = detect(path);
  if !kind.is_image() {
    return Err(Error::Unsupported { path: path.to_path_buf() });
  }
  let bytes = read_bounded(path, MAX_IMAGE_BYTES)?;
  let disk = Fingerprint::of(path).map_err(|source| Error::Read { path: path.to_path_buf(), source })?;
  let (format, width, height) =
    probe_image(&bytes, kind).map_err(|reason| Error::Decode { path: path.to_path_buf(), reason })?;
  Ok(LoadedImage { kind, format, bytes, width, height, disk })
}

/// A PDF read from disk. The bytes are shared: the renderer and the text
/// extractor both read them, and neither modifies them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedPdf {
  /// The file contents, unmodified.
  pub bytes: std::sync::Arc<Vec<u8>>,
  /// Source file fingerprint.
  pub disk: Fingerprint,
}

/// Read a PDF document: bounded bytes and a header check, no parse.
pub fn load_pdf(path: &Path) -> Result<LoadedPdf, Error> {
  if !detect(path).is_pdf() {
    return Err(Error::Unsupported { path: path.to_path_buf() });
  }
  let bytes = read_bounded(path, MAX_PDF_BYTES)?;
  if !bytes.starts_with(b"%PDF-") {
    return Err(Error::Pdf {
      reason: format!("{} is not a PDF file", path.display()),
    });
  }
  let disk = Fingerprint::of(path).map_err(|source| Error::Read { path: path.to_path_buf(), source })?;
  Ok(LoadedPdf { bytes: std::sync::Arc::new(bytes), disk })
}

/// Read the format and pixel size of image bytes already in memory.
///
/// # Errors
///
/// Returns a decoder message when the bytes are not a supported image.
pub fn probe_image(bytes: &[u8], kind: DocumentKind) -> Result<(ImageFormat, u32, u32), String> {
  if kind == DocumentKind::Svg {
    let (width, height) = svg_size(bytes);
    return Ok((ImageFormat::Svg, width, height));
  }
  let guessed = image::guess_format(bytes).map_err(|error| error.to_string())?;
  let format = ImageFormat::from_raster(guessed).ok_or_else(|| format!("{guessed:?} is not a supported format"))?;
  let mut reader = image::ImageReader::with_format(io::Cursor::new(bytes), guessed);
  reader.limits(decode_limits());
  let decoder = reader.into_decoder().map_err(|error| error.to_string())?;
  let (width, height) = decoder.dimensions();
  Ok((format, width, height))
}

/// Allocation limits shared by header probes and full decodes.
#[must_use]
pub fn decode_limits() -> image::Limits {
  let mut limits = image::Limits::no_limits();
  limits.max_alloc = Some(DECODE_ALLOC_LIMIT);
  limits
}

/// Read up to `limit` bytes from one open handle, refusing a larger file.
fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, Error> {
  let read_err = |source: io::Error| Error::Read { path: path.to_path_buf(), source };
  let file = File::open(path).map_err(read_err)?;
  let metadata = file.metadata().map_err(read_err)?;
  if !metadata.is_file() {
    return Err(read_err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file")));
  }
  let size = metadata.len();
  if size > limit {
    return Err(Error::TooLarge { path: path.to_path_buf(), size, limit });
  }
  let capacity = usize::try_from(size.min(limit)).unwrap_or(usize::MAX);
  let mut bytes = Vec::with_capacity(capacity);
  let read = file.take(limit + 1).read_to_end(&mut bytes).map_err(read_err)?;
  let read = u64::try_from(read).unwrap_or(u64::MAX);
  if read > limit {
    return Err(Error::TooLarge {
      path: path.to_path_buf(),
      size: read,
      limit,
    });
  }
  Ok(bytes)
}

/// usvg's fallback when an SVG declares no size.
const SVG_DEFAULT_SIZE: (u32, u32) = (100, 100);

/// Read the root `<svg>` size without an XML parser: `width`/`height` first,
/// then the `viewBox` extent, then usvg's own default.
fn svg_size(bytes: &[u8]) -> (u32, u32) {
  let Ok(text) = std::str::from_utf8(bytes) else {
    return SVG_DEFAULT_SIZE;
  };
  let Some(start) = text.find("<svg") else {
    return SVG_DEFAULT_SIZE;
  };
  let rest = text.get(start..).unwrap_or_default();
  let tag = rest.find('>').map_or(rest, |end| rest.get(..end).unwrap_or(rest));
  let width = attribute(tag, "width").and_then(parse_length);
  let height = attribute(tag, "height").and_then(parse_length);
  if let (Some(width), Some(height)) = (width, height) {
    return (width, height);
  }
  let view_box = attribute(tag, "viewBox").and_then(|value| {
    let mut numbers = value
      .split([' ', ',', '\t', '\n'])
      .filter(|part| !part.is_empty())
      .skip(2)
      .filter_map(parse_length);
    Some((numbers.next()?, numbers.next()?))
  });
  match (width, height, view_box) {
    (Some(width), None, Some((_, box_height))) => (width, box_height),
    (None, Some(height), Some((box_width, _))) => (box_width, height),
    (_, _, Some(size)) => size,
    _ => SVG_DEFAULT_SIZE,
  }
}

/// The value of `name="..."` or `name='...'` inside one start tag.
fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
  let mut rest = tag;
  loop {
    let at = rest.find(name)?;
    let before_is_space = rest
      .get(..at)
      .and_then(|head| head.chars().next_back())
      .is_none_or(char::is_whitespace);
    let after = rest.get(at + name.len()..)?;
    let after_trimmed = after.trim_start();
    if before_is_space && after_trimmed.starts_with('=') {
      let value = after_trimmed.get(1..)?.trim_start();
      let quote = value.chars().next()?;
      if quote == '"' || quote == '\'' {
        let value = value.get(1..)?;
        return value.find(quote).and_then(|end| value.get(..end));
      }
    }
    rest = after;
  }
}

/// A length OpenIt accepts for an SVG size: a positive number with an optional
/// `px`, rounded half up. Percentages and other units have no intrinsic size,
/// so they fall through to the `viewBox`.
fn parse_length(value: &str) -> Option<u32> {
  let value = value.trim();
  let number = value.strip_suffix("px").unwrap_or(value).trim();
  let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
  if !fraction.is_empty() && !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
    return None;
  }
  let whole: u32 = whole.parse().ok()?;
  let rounds_up = fraction.as_bytes().first().is_some_and(|digit| *digit >= b'5');
  let rounded = if rounds_up {
    whole.saturating_add(1)
  } else {
    whole
  };
  (rounded > 0).then_some(rounded)
}

#[cfg(test)]
mod tests {
  use std::fs;

  use super::{Error, ImageFormat, MAX_TEXT_BYTES, Revision, Snapshot, load_image, load_pdf, load_text};
  use crate::kind::DocumentKind;
  use crate::watch::Fingerprint;

  #[test]
  fn loads_markdown_with_its_kind() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.md");
    fs::write(&path, "# Title\n\nBody\n").unwrap();

    let loaded = load_text(&path).unwrap();

    assert_eq!(loaded.kind, DocumentKind::Markdown);
    assert_eq!(loaded.text, "# Title\n\nBody\n");
    assert_eq!(loaded.disk, Fingerprint::of(&path).unwrap());
  }

  #[test]
  fn rejects_files_over_the_text_cap_without_reading_them() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("huge.txt");
    let file = fs::File::create(&path).unwrap();
    file.set_len(MAX_TEXT_BYTES + 1).unwrap();

    let err = load_text(&path).unwrap_err();

    assert!(matches!(err, Error::TooLarge { size, .. } if size == MAX_TEXT_BYTES + 1));
  }

  #[test]
  fn rejects_invalid_utf8() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.txt");
    fs::write(&path, [0x66, 0xff, 0xfe, 0x67]).unwrap();

    assert!(matches!(load_text(&path).unwrap_err(), Error::NotUtf8 { .. }));
  }

  #[test]
  fn refuses_unsupported_kinds() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("photo.png");
    fs::write(&path, b"\x89PNG").unwrap();

    assert!(matches!(load_text(&path).unwrap_err(), Error::Unsupported { .. }));
  }

  #[cfg(unix)]
  #[test]
  fn refuses_a_fifo_without_a_writer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pipe.txt");
    assert!(std::process::Command::new("mkfifo").arg(&path).status().unwrap().success());
    let load_path = path;
    let (sender, receiver) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || sender.send(load_text(&load_path)).unwrap());

    let err = receiver.recv_timeout(std::time::Duration::from_secs(2)).unwrap().unwrap_err();
    handle.join().unwrap();

    assert!(matches!(
      err,
      Error::Read { source, .. } if source.kind() == std::io::ErrorKind::InvalidInput
    ));
  }

  #[test]
  fn missing_unsupported_file_is_a_read_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gone.png");

    assert!(matches!(load_text(&path).unwrap_err(), Error::Read { .. }));
  }

  #[test]
  fn accepts_a_file_at_exactly_the_text_cap() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("brim.txt");
    let file = fs::File::create(&path).unwrap();
    file.set_len(MAX_TEXT_BYTES).unwrap();

    let loaded = load_text(&path).unwrap();

    assert_eq!(u64::try_from(loaded.text.len()).unwrap(), MAX_TEXT_BYTES);
  }

  #[test]
  fn missing_file_is_a_read_error_naming_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nope.txt");

    let err = load_text(&path).unwrap_err();

    assert!(matches!(&err, Error::Read { path: p, .. } if p == &path));
    assert!(err.to_string().contains("nope.txt"));
  }

  #[test]
  fn load_image_reads_header_dimensions_without_decoding() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("p.png");
    image::RgbaImage::from_pixel(3, 2, image::Rgba([1, 2, 3, 4]))
      .save(&path)
      .unwrap();

    let loaded = load_image(&path).unwrap();

    assert_eq!((loaded.width, loaded.height), (3, 2));
    assert_eq!(loaded.format, ImageFormat::Png);
    assert_eq!(loaded.kind, DocumentKind::Image);
    assert_eq!(loaded.bytes, fs::read(&path).unwrap());
    assert_eq!(loaded.disk, Fingerprint::of(&path).unwrap());
  }

  #[test]
  fn load_image_reads_svg_size_from_the_root_element() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v.svg");
    fs::write(
      &path,
      br#"<svg xmlns="http://www.w3.org/2000/svg" width="40px" height="30"></svg>"#,
    )
    .unwrap();

    let loaded = load_image(&path).unwrap();

    assert_eq!((loaded.width, loaded.height), (40, 30));
    assert_eq!(loaded.format, ImageFormat::Svg);
    assert_eq!(loaded.kind, DocumentKind::Svg);
  }

  #[test]
  fn load_image_falls_back_to_the_view_box() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v.svg");
    fs::write(&path, br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 12"/>"#).unwrap();

    let loaded = load_image(&path).unwrap();

    assert_eq!((loaded.width, loaded.height), (24, 12));
  }

  #[test]
  fn load_image_rejects_bytes_that_are_not_a_supported_image() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.png");
    fs::write(&path, b"not a png at all").unwrap();

    let err = load_image(&path).unwrap_err();

    assert!(matches!(err, Error::Decode { .. }), "got {err:?}");
    assert!(err.to_string().contains("c.png"));
  }

  #[test]
  fn load_image_refuses_documents_without_an_image_reader() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.md");
    fs::write(&path, "# Title\n").unwrap();

    assert!(matches!(load_image(&path), Err(Error::Unsupported { .. })));
  }

  #[test]
  fn load_text_still_refuses_images() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("p.png");
    fs::write(&path, b"x").unwrap();

    assert!(matches!(load_text(&path), Err(Error::Unsupported { .. })));
  }

  #[test]
  fn load_pdf_reads_the_bytes_and_the_fingerprint() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.pdf");
    fs::write(&path, b"%PDF-1.4\n%%EOF\n").unwrap();

    let loaded = load_pdf(&path).unwrap();

    assert_eq!(loaded.bytes.as_slice(), b"%PDF-1.4\n%%EOF\n");
    assert_eq!(loaded.disk, crate::watch::Fingerprint::of(&path).unwrap());
  }

  #[test]
  fn load_pdf_rejects_bytes_without_the_pdf_header() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.pdf");
    fs::write(&path, b"hello").unwrap();

    assert!(matches!(load_pdf(&path), Err(Error::Pdf { .. })));
  }

  #[test]
  fn text_and_image_readers_refuse_pdfs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.pdf");
    fs::write(&path, b"%PDF-1.4\n").unwrap();

    assert!(matches!(load_text(&path), Err(Error::Unsupported { .. })));
    assert!(matches!(load_image(&path), Err(Error::Unsupported { .. })));
    assert!(matches!(
      load_pdf(dir.path().join("b.md").as_path()),
      Err(Error::Unsupported { .. })
    ));
  }

  #[test]
  fn revisions_increase_and_snapshots_keep_theirs() {
    let first = Revision::INITIAL;
    let second = first.next();
    assert!(second > first);

    let snapshot = Snapshot {
      revision: second,
      text: ropey::Rope::from_str("x"),
    };
    assert_eq!(snapshot.revision, second);
    assert_eq!(snapshot.text.to_string(), "x");
  }
}
