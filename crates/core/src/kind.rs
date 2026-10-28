//! What a path opens as. Decided from the filename; the PDF reader checks the
//! header before it parses.

use std::path::Path;

/// How OpenIt presents a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentKind {
  /// Rendered preview by default, editor on demand.
  Markdown,
  /// Editor with the named gpui-kit grammar, or plain text when `None`.
  Text {
    /// gpui-kit language name, when a grammar is compiled in.
    language: Option<&'static str>,
  },
  /// Raster image decoded by the `image` crate.
  Image,
  /// Vector image rasterized by resvg.
  Svg,
  /// Paged document rendered by hayro.
  Pdf,
  /// Nothing in OpenIt reads this file.
  Unsupported,
}

impl DocumentKind {
  /// The grammar name for syntax highlighting.
  pub const fn language(self) -> Option<&'static str> {
    match self {
      Self::Markdown => Some("markdown"),
      Self::Text { language } => language,
      Self::Image | Self::Svg | Self::Pdf | Self::Unsupported => None,
    }
  }

  /// Whether the document opens in the image viewer.
  pub const fn is_image(self) -> bool {
    matches!(self, Self::Image | Self::Svg)
  }

  /// Whether the document opens in the PDF reader.
  pub const fn is_pdf(self) -> bool {
    matches!(self, Self::Pdf)
  }
}

/// Filename extensions that open as Markdown.
pub const MARKDOWN_EXTENSIONS: &[&str] = &["md", "markdown", "mdx"];

/// Code extensions paired with their gpui-kit grammar name.
///
/// `jsdoc` is compiled in as an injection grammar, not a file type, so it has no mapping here.
pub(crate) const CODE_LANGUAGES: &[(&str, &str)] = &[
  ("rs", "rust"),
  ("ts", "typescript"),
  ("mts", "typescript"),
  ("cts", "typescript"),
  ("tsx", "tsx"),
  ("js", "javascript"),
  ("mjs", "javascript"),
  ("cjs", "javascript"),
  ("jsx", "javascript"),
  ("json", "json"),
  ("jsonc", "json"),
  ("json5", "json"),
  ("html", "html"),
  ("htm", "html"),
  ("css", "css"),
  ("scss", "css"),
  ("less", "css"),
  ("toml", "toml"),
  ("yaml", "yaml"),
  ("yml", "yaml"),
  ("py", "python"),
  ("go", "go"),
  ("sh", "bash"),
  ("bash", "bash"),
  ("zsh", "bash"),
  ("sql", "sql"),
  ("java", "java"),
  ("kt", "kotlin"),
  ("kts", "kotlin"),
  ("swift", "swift"),
  ("c", "c"),
  ("h", "c"),
  ("cpp", "cpp"),
  ("cc", "cpp"),
  ("cxx", "cpp"),
  ("hpp", "cpp"),
  ("cs", "csharp"),
  ("rb", "ruby"),
  ("php", "php"),
  ("lua", "lua"),
  ("scala", "scala"),
  ("ex", "elixir"),
  ("exs", "elixir"),
  ("graphql", "graphql"),
  ("gql", "graphql"),
  ("proto", "proto"),
  ("zig", "zig"),
  ("astro", "astro"),
  ("cmake", "cmake"),
  ("diff", "diff"),
  ("patch", "diff"),
  ("ejs", "ejs"),
  ("erb", "erb"),
  ("mk", "make"),
  ("svelte", "svelte"),
];

/// Text extensions with no dedicated grammar.
pub const PLAIN_TEXT_EXTENSIONS: &[&str] = &["txt", "text", "log", "csv", "env", "ini", "cfg", "conf", "xml"];

/// Code and plain-text extensions declared for packaging. Keep in lockstep with
/// the code-language table and [`PLAIN_TEXT_EXTENSIONS`].
pub const TEXT_EXTENSIONS: &[&str] = &[
  "rs", "ts", "mts", "cts", "tsx", "js", "mjs", "cjs", "jsx", "json", "jsonc", "json5", "html", "htm", "css", "scss",
  "less", "toml", "yaml", "yml", "py", "go", "sh", "bash", "zsh", "sql", "java", "kt", "kts", "swift", "c", "h", "cpp",
  "cc", "cxx", "hpp", "cs", "rb", "php", "lua", "scala", "ex", "exs", "graphql", "gql", "proto", "zig", "txt", "text",
  "log", "csv", "env", "ini", "cfg", "conf", "xml", "astro", "cmake", "diff", "patch", "ejs", "erb", "mk", "svelte",
];

/// Raster image extensions the image crate reads.
pub const IMAGE_EXTENSIONS: &[&str] = &[
  "png", "apng", "jpg", "jpeg", "jfif", "gif", "webp", "bmp", "ico", "cur", "tif", "tiff", "tga", "pbm", "pgm", "ppm",
  "pnm", "pam",
];

/// Vector image extensions resvg reads.
pub const SVG_EXTENSIONS: &[&str] = &["svg", "svgz"];

/// PDF extensions the reader accepts.
pub const PDF_EXTENSIONS: &[&str] = &["pdf"];

/// Extensions with no reader. Not declared as file associations.
pub const UNSUPPORTED_EXTENSIONS: &[&str] = &[
  "zip", "gz", "bz2", "xz", "zst", "7z", "rar", "tar", "exe", "dmg", "bin", "o", "so", "dylib", "dll", "class", "wasm",
  "heic", "heif", "avif", "jxl", "jp2", "psd", "exr", "dds", "ktx", "ktx2", "doc", "docx", "xls", "xlsx", "ppt",
  "pptx", "odt", "ods", "odp", "pages", "numbers", "key",
];

/// Every Markdown, text, image, SVG, and PDF extension `detect` names.
pub fn accepted_extensions() -> impl Iterator<Item = &'static str> {
  MARKDOWN_EXTENSIONS
    .iter()
    .copied()
    .chain(CODE_LANGUAGES.iter().map(|(ext, _)| *ext))
    .chain(PLAIN_TEXT_EXTENSIONS.iter().copied())
    .chain(IMAGE_EXTENSIONS.iter().copied())
    .chain(SVG_EXTENSIONS.iter().copied())
    .chain(PDF_EXTENSIONS.iter().copied())
}

/// Decide the kind from the file name.
pub fn detect(path: &Path) -> DocumentKind {
  let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
  if let Some(kind) = by_filename(name) {
    return kind;
  }
  let extension = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
  match extension.as_deref() {
    Some(ext) if MARKDOWN_EXTENSIONS.contains(&ext) => DocumentKind::Markdown,
    Some(ext) => by_extension(ext),
    None => DocumentKind::Text { language: None },
  }
}

fn by_filename(name: &str) -> Option<DocumentKind> {
  let language = match name {
    "Dockerfile" | "justfile" | "Justfile" => Some("bash"),
    "Makefile" | "makefile" | "GNUmakefile" => Some("make"),
    "CMakeLists.txt" => Some("cmake"),
    "LICENSE" | "README" | "CHANGELOG" | "NOTICE" => None,
    _ => return None,
  };
  Some(DocumentKind::Text { language })
}

fn by_extension(ext: &str) -> DocumentKind {
  if let Some((_, language)) = CODE_LANGUAGES.iter().find(|(candidate, _)| *candidate == ext) {
    return DocumentKind::Text { language: Some(*language) };
  }
  if PLAIN_TEXT_EXTENSIONS.contains(&ext) {
    return DocumentKind::Text { language: None };
  }
  if IMAGE_EXTENSIONS.contains(&ext) {
    return DocumentKind::Image;
  }
  if SVG_EXTENSIONS.contains(&ext) {
    return DocumentKind::Svg;
  }
  if PDF_EXTENSIONS.contains(&ext) {
    return DocumentKind::Pdf;
  }
  if UNSUPPORTED_EXTENSIONS.contains(&ext) {
    return DocumentKind::Unsupported;
  }
  DocumentKind::Text { language: None }
}

/// Evidence needed before clipboard text counts as Markdown.
const MARKDOWN_THRESHOLD: u32 = 2;

/// Decide the kind of text that has no file name, such as the clipboard: a
/// JSON object or array opens as JSON, clear Markdown as Markdown, anything
/// else as plain text.
pub fn sniff_text(text: &str) -> DocumentKind {
  let trimmed = text.trim();
  if is_json_container(trimmed) {
    return DocumentKind::Text { language: Some("json") };
  }
  if markdown_score(trimmed) >= MARKDOWN_THRESHOLD {
    return DocumentKind::Markdown;
  }
  DocumentKind::Text { language: None }
}

/// Whether `text` is one JSON object or array, and nothing else.
fn is_json_container(text: &str) -> bool {
  let wrapped = (text.starts_with('{') && text.ends_with('}')) || (text.starts_with('[') && text.ends_with(']'));
  wrapped && serde_json::from_str::<serde_json::Value>(text).is_ok()
}

/// How much of `text` is Markdown syntax that plain prose rarely contains.
/// Fences and tables count double; headings, lists, quotes, links, and bold
/// count once, so a lone `# comment` line stays plain text.
fn markdown_score(text: &str) -> u32 {
  let (mut heading, mut fence, mut table, mut quote) = (false, false, false, false);
  let mut list_items = 0_u32;
  for line in text.lines().map(str::trim_start) {
    if line.starts_with("```") || line.starts_with("~~~") {
      fence = true;
    } else if is_heading(line) {
      heading = true;
    } else if is_list_item(line) {
      list_items += 1;
    } else if line.starts_with("> ") {
      quote = true;
    } else if is_table_rule(line) {
      table = true;
    }
  }
  let link = text.contains('[') && text.contains("](");
  let bold = text.matches("**").nth(1).is_some();
  let weights = [
    (fence, 2),
    (table, 2),
    (heading, 1),
    (list_items >= 2, 1),
    (quote, 1),
    (link, 1),
    (bold, 1),
  ];
  weights.iter().filter(|(present, _)| *present).map(|(_, weight)| weight).sum()
}

/// An ATX heading: one to six `#` and a space.
fn is_heading(line: &str) -> bool {
  let rest = line.trim_start_matches('#');
  let hashes = line.len() - rest.len();
  (1..=6).contains(&hashes) && rest.starts_with(' ')
}

/// A bullet or numbered list item.
fn is_list_item(line: &str) -> bool {
  if ["- ", "* ", "+ "].iter().any(|marker| line.starts_with(marker)) {
    return true;
  }
  let rest = line.trim_start_matches(|ch: char| ch.is_ascii_digit());
  rest.len() < line.len() && rest.starts_with(". ")
}

/// The `| --- | --- |` rule row of a table.
fn is_table_rule(line: &str) -> bool {
  line.contains('|') && line.contains("--") && line.chars().all(|ch| matches!(ch, '|' | '-' | ':' | ' ' | '\t'))
}

#[cfg(test)]
mod tests {
  use std::path::Path;

  use super::{DocumentKind, detect};

  #[test]
  fn markdown_extensions_open_as_markdown() {
    assert_eq!(detect(Path::new("notes.md")), DocumentKind::Markdown);
    assert_eq!(detect(Path::new("README.markdown")), DocumentKind::Markdown);
    assert_eq!(detect(Path::new("page.MDX")), DocumentKind::Markdown);
  }

  #[test]
  fn known_code_extensions_map_to_a_grammar() {
    assert_eq!(detect(Path::new("main.rs")), DocumentKind::Text { language: Some("rust") });
    assert_eq!(detect(Path::new("app.tsx")), DocumentKind::Text { language: Some("tsx") });
    assert_eq!(detect(Path::new("Cargo.toml")), DocumentKind::Text { language: Some("toml") });
    assert_eq!(detect(Path::new("deploy.yml")), DocumentKind::Text { language: Some("yaml") });
    assert_eq!(detect(Path::new("run.sh")), DocumentKind::Text { language: Some("bash") });
  }

  #[test]
  fn well_known_filenames_without_extension_are_text() {
    assert_eq!(detect(Path::new("Dockerfile")), DocumentKind::Text { language: Some("bash") });
    assert_eq!(detect(Path::new("justfile")), DocumentKind::Text { language: Some("bash") });
    assert_eq!(detect(Path::new("LICENSE")), DocumentKind::Text { language: None });
  }

  #[test]
  fn additional_grammars_map_from_extension_and_filename() {
    assert_eq!(detect(Path::new("page.astro")), DocumentKind::Text { language: Some("astro") });
    assert_eq!(
      detect(Path::new("CMakeLists.txt")),
      DocumentKind::Text { language: Some("cmake") }
    );
    assert_eq!(detect(Path::new("build.cmake")), DocumentKind::Text { language: Some("cmake") });
    assert_eq!(detect(Path::new("changes.diff")), DocumentKind::Text { language: Some("diff") });
    assert_eq!(
      detect(Path::new("changes.patch")),
      DocumentKind::Text { language: Some("diff") }
    );
    assert_eq!(detect(Path::new("view.ejs")), DocumentKind::Text { language: Some("ejs") });
    assert_eq!(detect(Path::new("form.erb")), DocumentKind::Text { language: Some("erb") });
    assert_eq!(detect(Path::new("App.svelte")), DocumentKind::Text { language: Some("svelte") });
    assert_eq!(detect(Path::new("Makefile")), DocumentKind::Text { language: Some("make") });
    assert_eq!(detect(Path::new("makefile")), DocumentKind::Text { language: Some("make") });
    assert_eq!(detect(Path::new("GNUmakefile")), DocumentKind::Text { language: Some("make") });
    assert_eq!(detect(Path::new("rules.mk")), DocumentKind::Text { language: Some("make") });
  }

  #[test]
  fn json_variants_keep_the_json_grammar() {
    assert_eq!(
      detect(Path::new("settings.jsonc")),
      DocumentKind::Text { language: Some("json") }
    );
    assert_eq!(
      detect(Path::new("tsconfig.json")),
      DocumentKind::Text { language: Some("json") }
    );
  }

  #[test]
  fn image_extensions_open_as_images() {
    assert_eq!(detect(Path::new("photo.png")), DocumentKind::Image);
    assert_eq!(detect(Path::new("photo.JPG")), DocumentKind::Image);
    assert_eq!(detect(Path::new("loop.gif")), DocumentKind::Image);
    assert_eq!(detect(Path::new("shot.webp")), DocumentKind::Image);
    assert_eq!(detect(Path::new("old.bmp")), DocumentKind::Image);
    assert_eq!(detect(Path::new("app.ico")), DocumentKind::Image);
    assert_eq!(detect(Path::new("pointer.cur")), DocumentKind::Image);
    assert_eq!(detect(Path::new("scan.tiff")), DocumentKind::Image);
    assert_eq!(detect(Path::new("sprite.tga")), DocumentKind::Image);
    assert_eq!(detect(Path::new("mask.pgm")), DocumentKind::Image);
  }

  #[test]
  fn svg_extensions_open_as_vector_images() {
    assert_eq!(detect(Path::new("logo.svg")), DocumentKind::Svg);
    assert_eq!(detect(Path::new("logo.svgz")), DocumentKind::Svg);
  }

  #[test]
  fn formats_without_a_reader_stay_unsupported() {
    assert_eq!(detect(Path::new("bundle.dmg")), DocumentKind::Unsupported);
    assert_eq!(detect(Path::new("report.docx")), DocumentKind::Unsupported);
    assert_eq!(detect(Path::new("archive.zip")), DocumentKind::Unsupported);
    assert_eq!(detect(Path::new("photo.heic")), DocumentKind::Unsupported);
    assert_eq!(detect(Path::new("photo.avif")), DocumentKind::Unsupported);
    assert_eq!(detect(Path::new("art.psd")), DocumentKind::Unsupported);
  }

  #[test]
  fn pdf_extensions_open_in_the_reader() {
    assert_eq!(detect(Path::new("paper.pdf")), DocumentKind::Pdf);
    assert_eq!(detect(Path::new("PAPER.PDF")), DocumentKind::Pdf);
    assert!(DocumentKind::Pdf.is_pdf());
    assert!(!DocumentKind::Pdf.is_image());
  }

  #[test]
  fn language_names_the_grammar_for_each_kind() {
    assert_eq!(DocumentKind::Markdown.language(), Some("markdown"));
    assert_eq!(DocumentKind::Text { language: Some("rust") }.language(), Some("rust"));
    assert_eq!(DocumentKind::Text { language: None }.language(), None);
    assert_eq!(DocumentKind::Image.language(), None);
    assert_eq!(DocumentKind::Svg.language(), None);
    assert_eq!(DocumentKind::Pdf.language(), None);
    assert_eq!(DocumentKind::Unsupported.language(), None);
  }

  #[test]
  fn clipboard_json_opens_as_json() {
    assert_eq!(super::sniff_text("{\"a\": 1}\n"), DocumentKind::Text { language: Some("json") });
    assert_eq!(super::sniff_text(" [1, 2, 3] "), DocumentKind::Text { language: Some("json") });
    assert_eq!(super::sniff_text("{not json}"), DocumentKind::Text { language: None });
  }

  #[test]
  fn clipboard_markdown_needs_more_than_one_hint() {
    let note = "# Clip title\n\nSome **markdown** text\n\n- a\n- b";
    assert_eq!(super::sniff_text(note), DocumentKind::Markdown);
    assert_eq!(super::sniff_text("```rust\nlet x = 1;\n```"), DocumentKind::Markdown);
    assert_eq!(
      super::sniff_text("# just a shell comment\nls"),
      DocumentKind::Text { language: None }
    );
    assert_eq!(super::sniff_text("[link](x)"), DocumentKind::Text { language: None });
    assert_eq!(super::sniff_text("plain words"), DocumentKind::Text { language: None });
  }
}
