//! Uncommitted changes against `HEAD`, read through the `git` binary.
//!
//! Git is optional: a missing binary, a path outside any repository, or a git
//! failure all read as "no changes" and are logged at `debug`. Both queries block
//! on a child process, so callers run them off the UI thread.

use std::collections::HashSet;
use std::path::Path;
use std::process::{Command, Stdio};

/// Names of the files directly inside `dir` that differ from `HEAD`, staged and
/// unstaged changes together. Untracked files and files in subdirectories are
/// not included.
pub fn changed_files(dir: &Path) -> HashSet<String> {
  if !in_repository(dir) {
    return HashSet::new();
  }
  let Some(output) = run_git(dir, &["diff", "HEAD", "--name-only", "-z", "--relative", "--", "."]) else {
    return HashSet::new();
  };
  output
    .split(|byte| *byte == 0)
    .filter(|name| !name.is_empty() && !name.contains(&b'/'))
    .map(|name| String::from_utf8_lossy(name).into_owned())
    .collect()
}

/// The patch of `path` against `HEAD`, staged and unstaged changes together, or
/// `None` when the file has no changes or git cannot answer.
pub fn patch(path: &Path) -> Option<String> {
  let dir = path.parent()?;
  let name = path.file_name()?;
  if !in_repository(dir) {
    return None;
  }
  let name = name.to_str()?;
  let output = run_git(
    dir,
    &["diff", "HEAD", "--no-color", "--no-ext-diff", "--no-textconv", "--", name],
  )?;
  if output.is_empty() {
    return None;
  }
  match String::from_utf8(output) {
    Ok(text) => Some(text),
    Err(error) => {
      tracing::debug!(%error, path = %path.display(), "git patch is not UTF-8");
      None
    },
  }
}

/// Whether `dir` or an ancestor holds a `.git` entry. Checked before spawning so
/// files outside a repository never start a process.
fn in_repository(dir: &Path) -> bool {
  dir.ancestors().any(|ancestor| ancestor.join(".git").exists())
}

/// Run git in `dir` and return its stdout, or `None` with a `debug` log on any failure.
///
/// `core.fsmonitor` is forced off so a repository's own configuration cannot run
/// a command just because a file in it was opened.
fn run_git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
  let output = Command::new("git")
    .arg("-C")
    .arg(dir)
    .args(["-c", "core.fsmonitor=false"])
    .args(args)
    .env("GIT_OPTIONAL_LOCKS", "0")
    .stdin(Stdio::null())
    .output();
  match output {
    Ok(output) if output.status.success() => Some(output.stdout),
    Ok(output) => {
      tracing::debug!(
        status = %output.status,
        stderr = %String::from_utf8_lossy(&output.stderr).trim(),
        dir = %dir.display(),
        "git query failed"
      );
      None
    },
    Err(error) => {
      tracing::debug!(%error, "git unavailable");
      None
    },
  }
}

#[cfg(test)]
mod tests {
  use std::fs;
  use std::path::Path;
  use std::process::Command;

  use super::{changed_files, patch};

  fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
      .arg("-C")
      .arg(dir)
      .args([
        "-c",
        "user.name=OpenIt",
        "-c",
        "user.email=openit@example.org",
        "-c",
        "commit.gpgsign=false",
      ])
      .args(args)
      .env("GIT_CONFIG_GLOBAL", "/dev/null")
      .env("GIT_CONFIG_NOSYSTEM", "1")
      .stdout(std::process::Stdio::null())
      .status()
      .unwrap();
    assert!(status.success(), "git {args:?} failed");
  }

  /// A repository with `clean.txt`, `staged.txt`, `unstaged.txt`, `both.txt`,
  /// and `sub/nested.txt` committed, then changed as their names say, plus an
  /// untracked `new.txt` and a changed `sub/nested.txt`.
  fn repository() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    fs::create_dir(root.join("sub")).unwrap();
    for name in ["clean.txt", "staged.txt", "unstaged.txt", "both.txt", "sub/nested.txt"] {
      fs::write(root.join(name), "one\ntwo\n").unwrap();
    }
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "initial"]);
    fs::write(root.join("staged.txt"), "one\nstaged\n").unwrap();
    git(root, &["add", "staged.txt"]);
    fs::write(root.join("both.txt"), "one\nstaged\n").unwrap();
    git(root, &["add", "both.txt"]);
    fs::write(root.join("both.txt"), "one\nstaged\nunstaged\n").unwrap();
    fs::write(root.join("unstaged.txt"), "one\nunstaged\n").unwrap();
    fs::write(root.join("sub/nested.txt"), "changed\n").unwrap();
    fs::write(root.join("new.txt"), "untracked\n").unwrap();
    dir
  }

  #[test]
  fn changed_files_lists_staged_and_unstaged_direct_children_only() {
    let dir = repository();

    let mut changed: Vec<String> = changed_files(dir.path()).into_iter().collect();
    changed.sort();

    assert_eq!(changed, ["both.txt", "staged.txt", "unstaged.txt"]);
  }

  #[test]
  fn changed_files_in_a_subdirectory_are_relative_to_it() {
    let dir = repository();

    let changed: Vec<String> = changed_files(&dir.path().join("sub")).into_iter().collect();

    assert_eq!(changed, ["nested.txt"]);
  }

  #[test]
  fn patch_covers_staged_and_unstaged_changes_against_head() {
    let dir = repository();

    let both = patch(&dir.path().join("both.txt")).unwrap();
    assert!(both.contains("-two\n+staged\n+unstaged\n"), "{both}");
    assert!(patch(&dir.path().join("staged.txt")).unwrap().contains("+staged"));
    assert!(patch(&dir.path().join("unstaged.txt")).unwrap().contains("+unstaged"));
  }

  #[test]
  fn clean_and_untracked_files_have_no_patch() {
    let dir = repository();

    assert_eq!(patch(&dir.path().join("clean.txt")), None);
    assert_eq!(patch(&dir.path().join("new.txt")), None);
  }

  #[test]
  fn a_directory_outside_any_repository_has_no_changes() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("file.txt"), "text\n").unwrap();

    assert!(changed_files(dir.path()).is_empty());
    assert_eq!(patch(&dir.path().join("file.txt")), None);
  }

  #[test]
  fn a_repository_without_commits_has_no_changes() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    fs::write(dir.path().join("file.txt"), "text\n").unwrap();
    git(dir.path(), &["add", "file.txt"]);

    assert!(changed_files(dir.path()).is_empty());
    assert_eq!(patch(&dir.path().join("file.txt")), None);
  }
}
