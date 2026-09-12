//! One-directory listing, path query parsing, and fuzzy ranking for nearby files.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::error::Error;
use crate::kind::{DocumentKind, detect};

/// Maximum number of ranked rows returned to the picker.
const RANK_LIMIT: usize = 100;

/// A directory entry the nearby-files picker can show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
  /// File or directory name without a trailing slash.
  pub name: String,
  /// Whether this entry is a directory after following symbolic links.
  pub is_dir: bool,
  /// Kind from the file name.
  pub kind: DocumentKind,
}

/// Split `input` at its last `/` into a directory resolved against `root` and a search needle.
///
/// The left side may contain `..`, start with `~` for the home directory, or be absolute.
/// A trailing `/` yields an empty needle. This function does not touch the filesystem.
pub fn parse(input: &str, root: &Path) -> (PathBuf, String) {
  match input.rsplit_once('/') {
    None => (root.to_path_buf(), input.to_owned()),
    Some(("", needle)) => (PathBuf::from("/"), needle.to_owned()),
    Some((left, needle)) => (normalize(&resolve_dir(left, root)), needle.to_owned()),
  }
}

/// Collapse `.` and `..` components lexically, so the picker shows `/tmp/` rather
/// than `/tmp/dir/../`. Symbolic links are not consulted; the listing that follows
/// reads whatever the collapsed path names.
fn normalize(path: &Path) -> PathBuf {
  use std::path::Component;
  let mut out = PathBuf::new();
  for component in path.components() {
    match component {
      Component::CurDir => {},
      Component::ParentDir => match out.components().next_back() {
        Some(Component::Normal(_)) => {
          out.pop();
        },
        Some(Component::RootDir | Component::Prefix(_)) => {},
        _ => out.push(component),
      },
      other => out.push(other),
    }
  }
  out
}

/// One non-recursive listing of `dir`.
///
/// Unsupported files are omitted. Dotfiles and dot-directories are omitted unless
/// `show_dotfiles` is true. Directories sort first, then files, each group by name
/// case-insensitively. An unreadable directory is an error, never an empty list.
pub fn list(dir: &Path, show_dotfiles: bool) -> Result<Vec<Entry>, Error> {
  let reader = fs::read_dir(dir).map_err(|source| browse_error(dir, source))?;
  let mut listed = Vec::new();
  for entry in reader {
    let entry = entry.map_err(|source| browse_error(dir, source))?;
    if let Some(item) = collect_entry(&entry.path(), show_dotfiles) {
      listed.push(item);
    }
  }
  listed.sort_by_key(|item| (!item.is_dir, item.name.to_lowercase()));
  Ok(listed)
}

/// A nearby-files row after fuzzy ranking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ranked {
  /// The listing entry.
  pub entry: Entry,
  /// Character indices in the entry name that matched the needle.
  pub positions: Vec<u32>,
}

impl Ranked {
  /// Displayed name. Directories keep a trailing `/`.
  pub fn display_name(&self) -> String {
    if self.entry.is_dir {
      format!("{}/", self.entry.name)
    } else {
      self.entry.name.clone()
    }
  }
}

/// Rank `entries` by fuzzy match against `needle`.
///
/// An empty needle keeps listing order and yields no positions. Otherwise nucleo
/// scores each entry name in one pool. Tighter matches (smaller index span) rank
/// first, then higher nucleo score, then name. The result is capped at 100 rows.
pub fn rank(entries: &[Entry], needle: &str) -> Vec<Ranked> {
  if needle.is_empty() {
    return entries
      .iter()
      .take(RANK_LIMIT)
      .map(|entry| Ranked {
        entry: entry.clone(),
        positions: Vec::new(),
      })
      .collect();
  }

  let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
  let atom = Atom::new(needle, CaseMatching::Ignore, Normalization::Smart, AtomKind::Fuzzy, false);
  let mut scored = Vec::with_capacity(entries.len());
  let mut buf = Vec::new();
  for entry in entries {
    let mut indices = Vec::new();
    let haystack = Utf32Str::new(&entry.name, &mut buf);
    if let Some(score) = atom.indices(haystack, &mut matcher, &mut indices) {
      scored.push((score, Ranked { entry: entry.clone(), positions: indices }));
    }
    buf.clear();
  }
  scored.sort_by(|(score_a, ranked_a), (score_b, ranked_b)| {
    match_span(&ranked_a.positions)
      .cmp(&match_span(&ranked_b.positions))
      .then_with(|| score_b.cmp(score_a))
      .then_with(|| ranked_a.entry.name.to_lowercase().cmp(&ranked_b.entry.name.to_lowercase()))
  });
  scored.truncate(RANK_LIMIT);
  scored.into_iter().map(|(_, ranked)| ranked).collect()
}

fn match_span(positions: &[u32]) -> u32 {
  positions
    .last()
    .zip(positions.first())
    .map_or(0, |(last, first)| last.saturating_sub(*first))
}

fn resolve_dir(left: &str, root: &Path) -> PathBuf {
  if let Some(expanded) = expand_tilde(left) {
    return expanded;
  }
  let path = Path::new(left);
  if path.is_absolute() {
    path.to_path_buf()
  } else {
    root.join(left)
  }
}

fn expand_tilde(path: &str) -> Option<PathBuf> {
  if path == "~" {
    return Some(dirs::home_dir().unwrap_or_else(|| PathBuf::from("~")));
  }
  let rest = path.strip_prefix("~/")?;
  Some(dirs::home_dir().map_or_else(|| PathBuf::from(path), |home| home.join(rest)))
}

fn collect_entry(path: &Path, show_dotfiles: bool) -> Option<Entry> {
  let name = path.file_name()?.to_str()?.to_owned();
  if !show_dotfiles && name.starts_with('.') {
    return None;
  }
  let metadata = match fs::metadata(path) {
    Ok(metadata) => metadata,
    Err(source) => {
      tracing::debug!(path = %path.display(), %source, "skipping unlistable entry");
      return None;
    },
  };
  let is_dir = metadata.is_dir();
  let kind = detect(path);
  if !is_dir && kind == DocumentKind::Unsupported {
    return None;
  }
  Some(Entry { name, is_dir, kind })
}

fn browse_error(dir: &Path, source: io::Error) -> Error {
  tracing::debug!(path = %dir.display(), %source, "directory is unreadable");
  Error::Browse { path: dir.to_path_buf(), source }
}

#[cfg(test)]
mod tests {
  use std::fs;
  use std::path::{Path, PathBuf};

  use super::{Entry, Ranked, list, parse, rank};
  use crate::kind::DocumentKind;

  fn display_names(entries: &[Entry]) -> Vec<String> {
    entries
      .iter()
      .map(|entry| {
        if entry.is_dir {
          format!("{}/", entry.name)
        } else {
          entry.name.clone()
        }
      })
      .collect()
  }

  fn fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(".hidden"), "").unwrap();
    fs::write(dir.path().join("notes.md"), "").unwrap();
    fs::write(dir.path().join("report.docx"), "").unwrap();
    fs::create_dir(dir.path().join("sub")).unwrap();
    let path = dir.path().to_path_buf();
    (dir, path)
  }

  #[test]
  fn listing_hides_dotfiles_and_unsupported_files() {
    let (_keep, path) = fixture();
    let listed = list(&path, false).unwrap();
    assert_eq!(display_names(&listed), ["sub/", "notes.md"]);
    assert!(listed[0].is_dir);
    assert_eq!(listed[1].kind, DocumentKind::Markdown);
  }

  #[test]
  fn listing_includes_dotfiles_when_requested() {
    let (_keep, path) = fixture();
    let listed = list(&path, true).unwrap();
    assert_eq!(display_names(&listed), ["sub/", ".hidden", "notes.md"]);
  }

  #[test]
  fn parse_splits_at_the_last_slash() {
    let root = Path::new("/tmp/root");
    assert_eq!(parse("../sub/rep", root), (PathBuf::from("/tmp/sub"), "rep".to_owned()));
    assert_eq!(parse("../../../up/", root), (PathBuf::from("/up"), String::new()));
    assert_eq!(parse("./a/./b/", root), (PathBuf::from("/tmp/root/a/b"), String::new()));
    assert_eq!(parse("/tmp/", root), (PathBuf::from("/tmp"), String::new()));
    let home = dirs::home_dir().expect("home directory");
    assert_eq!(parse("~/x", root), (home, "x".to_owned()));
  }

  #[cfg(unix)]
  #[test]
  fn unreadable_directory_returns_an_error() {
    use crate::error::Error;
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let locked = dir.path().join("locked");
    fs::create_dir(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let result = list(&locked, false);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
    match result {
      Err(Error::Browse { path, source }) => {
        assert_eq!(path, locked);
        assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
      },
      other => panic!("expected Browse, got {other:?}"),
    }
  }

  fn markdown(name: &str) -> Entry {
    Entry {
      name: name.to_owned(),
      is_dir: false,
      kind: DocumentKind::Markdown,
    }
  }

  #[test]
  fn rank_caps_at_one_hundred() {
    let entries: Vec<Entry> = (0..300).map(|i| markdown(&format!("file-{i:03}.md"))).collect();
    assert_eq!(rank(&entries, "file").len(), 100);
    assert_eq!(rank(&entries, "").len(), 100);
  }

  #[test]
  fn rank_places_readme_above_draft_for_rdm() {
    let entries = vec![markdown("readme-draft.md"), markdown("README.md")];
    let ranked = rank(&entries, "rdm");
    assert_eq!(ranked[0].entry.name, "README.md");
    assert_eq!(ranked[1].entry.name, "readme-draft.md");
    let chars: Vec<char> = ranked[0].entry.name.chars().collect();
    let matched: Vec<char> = ranked[0]
      .positions
      .iter()
      .map(|&index| chars[usize::try_from(index).unwrap()])
      .collect();
    assert_eq!(matched[0], 'R');
    let lowered: String = matched.into_iter().collect::<String>().to_ascii_lowercase();
    assert_eq!(lowered, "rdm");
  }

  #[test]
  fn empty_needle_keeps_listing_order() {
    let entries = vec![
      markdown("zeta.md"),
      Entry {
        name: "alpha".to_owned(),
        is_dir: true,
        kind: DocumentKind::Text { language: None },
      },
      markdown("beta.md"),
    ];
    let ranked = rank(&entries, "");
    assert_eq!(
      ranked.iter().map(Ranked::display_name).collect::<Vec<_>>(),
      ["zeta.md", "alpha/", "beta.md"]
    );
    assert!(ranked.iter().all(|row| row.positions.is_empty()));
  }
}
