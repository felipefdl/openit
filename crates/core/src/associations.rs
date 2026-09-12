//! Packager file associations and `file://` URL decoding.
//!
//! `FILE_ASSOCIATIONS` is the list `crates/app/Cargo.toml` copies for cargo-packager.

use std::path::PathBuf;

use crate::kind::{IMAGE_EXTENSIONS, MARKDOWN_EXTENSIONS, PDF_EXTENSIONS, SVG_EXTENSIONS, TEXT_EXTENSIONS};

/// Role the packaged app claims for a group of extensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssociationRole {
  /// OpenIt writes this type.
  Editor,
  /// OpenIt displays this type and does not save it in place.
  Viewer,
}

/// One cargo-packager file-association group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileAssociation {
  /// `CFBundleTypeName` on macOS.
  pub name: &'static str,
  /// Extensions without a leading dot.
  pub extensions: &'static [&'static str],
  /// Packager `role`.
  pub role: AssociationRole,
  /// Linux `.desktop` MIME type for the group.
  pub mime_type: &'static str,
}

/// Declared associations. Copied into `crates/app/Cargo.toml`.
pub const FILE_ASSOCIATIONS: &[FileAssociation] = &[
  FileAssociation {
    name: "Markdown",
    extensions: MARKDOWN_EXTENSIONS,
    role: AssociationRole::Editor,
    mime_type: "text/markdown",
  },
  FileAssociation {
    name: "Plain text",
    extensions: TEXT_EXTENSIONS,
    role: AssociationRole::Editor,
    mime_type: "text/plain",
  },
  FileAssociation {
    name: "Image",
    extensions: IMAGE_EXTENSIONS,
    role: AssociationRole::Editor,
    mime_type: "image/png",
  },
  FileAssociation {
    name: "SVG",
    extensions: SVG_EXTENSIONS,
    role: AssociationRole::Editor,
    mime_type: "image/svg+xml",
  },
  FileAssociation {
    name: "PDF",
    extensions: PDF_EXTENSIONS,
    role: AssociationRole::Viewer,
    mime_type: "application/pdf",
  },
];

/// Every extension the packaged app declares.
pub fn declared_extensions() -> impl Iterator<Item = &'static str> {
  FILE_ASSOCIATIONS.iter().flat_map(|group| group.extensions.iter().copied())
}

/// Percent-decoded local paths from OS open-URL events. Other schemes are logged and dropped.
pub fn paths_from_open_urls<I, S>(urls: I) -> Vec<PathBuf>
where
  I: IntoIterator<Item = S>,
  S: AsRef<str>,
{
  let mut paths = Vec::new();
  for url in urls {
    let url = url.as_ref();
    match url::Url::parse(url) {
      Ok(parsed) if parsed.scheme() == "file" => {
        if let Ok(path) = parsed.to_file_path() {
          paths.push(path);
        } else {
          tracing::info!(url, "ignored file url that is not a local path");
        }
      },
      Ok(parsed) => tracing::info!(scheme = parsed.scheme(), url, "ignored open url"),
      Err(_) => tracing::info!(url, "ignored unreadable open url"),
    }
  }
  paths
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeSet;
  use std::path::PathBuf;

  use super::{AssociationRole, FILE_ASSOCIATIONS, declared_extensions, paths_from_open_urls};
  use crate::kind::{
    CODE_LANGUAGES, DocumentKind, IMAGE_EXTENSIONS, MARKDOWN_EXTENSIONS, PDF_EXTENSIONS, PLAIN_TEXT_EXTENSIONS,
    SVG_EXTENSIONS, TEXT_EXTENSIONS, UNSUPPORTED_EXTENSIONS, accepted_extensions, detect,
  };

  fn set(items: impl IntoIterator<Item = &'static str>) -> BTreeSet<&'static str> {
    items.into_iter().collect()
  }

  #[test]
  fn declared_extensions_are_not_unsupported() {
    for ext in declared_extensions() {
      let path = PathBuf::from(format!("file.{ext}"));
      assert_ne!(
        detect(&path),
        DocumentKind::Unsupported,
        "{ext} is declared but detect refuses it"
      );
    }
  }

  #[test]
  fn every_accepted_kind_extension_is_declared() {
    let declared = set(declared_extensions());
    let accepted = set(accepted_extensions());
    assert_eq!(declared, accepted);
  }

  #[test]
  fn text_group_matches_code_and_plain_tables() {
    let listed = set(TEXT_EXTENSIONS.iter().copied());
    let from_tables = set(
      CODE_LANGUAGES
        .iter()
        .map(|(ext, _)| *ext)
        .chain(PLAIN_TEXT_EXTENSIONS.iter().copied()),
    );
    assert_eq!(listed, from_tables);
  }

  #[test]
  fn unsupported_extensions_are_not_declared() {
    let declared = set(declared_extensions());
    for ext in UNSUPPORTED_EXTENSIONS {
      assert!(!declared.contains(ext), "{ext} is Unsupported and must not be declared");
    }
  }

  #[test]
  fn groups_cover_each_kind_table() {
    let expected = [
      (MARKDOWN_EXTENSIONS, AssociationRole::Editor),
      (TEXT_EXTENSIONS, AssociationRole::Editor),
      (IMAGE_EXTENSIONS, AssociationRole::Editor),
      (SVG_EXTENSIONS, AssociationRole::Editor),
      (PDF_EXTENSIONS, AssociationRole::Viewer),
    ];
    assert_eq!(FILE_ASSOCIATIONS.len(), expected.len());
    for (group, (extensions, role)) in FILE_ASSOCIATIONS.iter().zip(expected) {
      assert_eq!(group.extensions, extensions);
      assert_eq!(group.role, role);
    }
  }

  #[test]
  fn packager_toml_lists_the_declared_extensions() {
    let groups: Vec<toml::Table> = include_str!("../../app/Cargo.toml")
      .split("[[package.metadata.packager.file-associations]]")
      .skip(1)
      .map(|body| body.parse().unwrap())
      .collect();
    let mut toml_exts = BTreeSet::new();
    assert_eq!(groups.len(), FILE_ASSOCIATIONS.len());
    for (group, declared) in groups.iter().zip(FILE_ASSOCIATIONS) {
      let exts = group.get("extensions").and_then(toml::Value::as_array).unwrap();
      let group_exts: Vec<&str> = exts.iter().filter_map(toml::Value::as_str).collect();
      assert_eq!(group_exts, declared.extensions);
      toml_exts.extend(group_exts);
      let role = group.get("role").and_then(toml::Value::as_str).unwrap_or("editor");
      let expected_role = match declared.role {
        AssociationRole::Editor => "editor",
        AssociationRole::Viewer => "viewer",
      };
      assert_eq!(role, expected_role, "{}", declared.name);
      let mime = group
        .get("mime-type")
        .or_else(|| group.get("mime_type"))
        .or_else(|| group.get("mimeType"))
        .and_then(toml::Value::as_str);
      assert_eq!(mime, Some(declared.mime_type), "{}", declared.name);
    }
    assert_eq!(toml_exts, set(declared_extensions()));
  }

  #[test]
  fn file_urls_decode_to_paths() {
    let paths = paths_from_open_urls(["file:///tmp/a%20b.md", "https://example.com/x.md"]);
    assert_eq!(paths, vec![PathBuf::from("/tmp/a b.md")]);
  }
}
