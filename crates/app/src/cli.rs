//! Command-line argument parsing for the OpenIt binary.

use std::ffi::OsStr;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Outcome of reading argv after the program name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Process {
  /// Print and stop with this status.
  Exit(u8),
  /// Continue as the application with these surviving paths.
  Launch(Vec<PathBuf>),
}

/// Parsed command line after the program name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
  /// Print usage and exit 0.
  Help,
  /// Print the crate version and exit 0.
  Version,
  /// Open the surviving files.
  Open(ResolvedPaths),
}

/// Path operands after existence checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedPaths {
  /// Absolute surviving files, in argument order.
  pub paths: Vec<PathBuf>,
  /// Stderr lines for dropped operands.
  pub errors: Vec<String>,
  /// True when at least one path operand was given.
  pub had_operands: bool,
}

impl ResolvedPaths {
  /// Paths were given and every one was dropped.
  pub const fn is_empty_after_operands(&self) -> bool {
    self.had_operands && self.paths.is_empty()
  }
}

/// A token that starts with `-` and is not a known option.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnknownFlag(pub String);

/// Parse `args` (without argv0) against `cwd`.
pub(crate) fn parse<I, S>(args: I, cwd: &Path) -> Result<Command, UnknownFlag>
where
  I: IntoIterator<Item = S>,
  S: AsRef<OsStr>,
{
  let mut end_of_flags = false;
  let mut paths = Vec::new();
  let mut errors = Vec::new();
  let mut had_operands = false;

  for arg in args {
    let arg = arg.as_ref();
    if !end_of_flags {
      if arg == "--" {
        end_of_flags = true;
        continue;
      }
      if arg == "--help" {
        return Ok(Command::Help);
      }
      if arg == "--version" {
        return Ok(Command::Version);
      }
      if starts_with_dash(arg) {
        return Err(UnknownFlag(arg.to_string_lossy().into_owned()));
      }
    }
    had_operands = true;
    resolve_operand(arg, cwd, &mut paths, &mut errors);
  }

  Ok(Command::Open(ResolvedPaths { paths, errors, had_operands }))
}

/// Read the process arguments and write help, version, usage, or path errors.
pub(crate) fn from_env() -> Process {
  let cwd = match std::env::current_dir() {
    Ok(cwd) => cwd,
    Err(error) => {
      let _ = writeln!(io::stderr(), "openit: could not read the current directory: {error}");
      return Process::Exit(1);
    },
  };

  match parse(std::env::args_os().skip(1), &cwd) {
    Ok(Command::Help) => {
      let _ = write_help(&mut io::stdout());
      Process::Exit(0)
    },
    Ok(Command::Version) => {
      let _ = write_version(&mut io::stdout());
      Process::Exit(0)
    },
    Ok(Command::Open(resolved)) => {
      let mut stderr = io::stderr();
      for line in &resolved.errors {
        let _ = writeln!(stderr, "{line}");
      }
      if resolved.is_empty_after_operands() {
        Process::Exit(1)
      } else {
        Process::Launch(resolved.paths)
      }
    },
    Err(UnknownFlag(flag)) => {
      let mut stderr = io::stderr();
      let _ = writeln!(stderr, "openit: unknown option {flag}");
      let _ = write_usage(&mut stderr);
      Process::Exit(2)
    },
  }
}

pub(crate) fn write_help(out: &mut impl Write) -> io::Result<()> {
  writeln!(out, "Usage: openit [path ...]")?;
  writeln!(out, "       openit --help")?;
  writeln!(out, "       openit --version")?;
  Ok(())
}

pub(crate) fn write_version(out: &mut impl Write) -> io::Result<()> {
  writeln!(out, "OpenIt {}", env!("CARGO_PKG_VERSION"))
}

pub(crate) fn write_usage(out: &mut impl Write) -> io::Result<()> {
  writeln!(out, "Usage: openit [path ...]")
}

#[cfg_attr(
  not(any(unix, test)),
  expect(dead_code, reason = "command-link relaunch is unix-only")
)]
const PRODUCT_BIN: &str = "OpenIt";

/// Whether this process was started as `oi`/`openit` while the file on disk is `OpenIt`.
#[cfg_attr(
  not(any(unix, test)),
  expect(dead_code, reason = "command-link relaunch is unix-only")
)]
pub(crate) fn should_relaunch_as_openit(invoked: &Path, resolved: &Path) -> bool {
  resolved.file_name() == Some(OsStr::new(PRODUCT_BIN)) && invoked.file_name() != Some(OsStr::new(PRODUCT_BIN))
}

/// Start `OpenIt` detached and exit, so the shell prompt comes back at once, the way
/// `open` behaves. The Dock and application menu then use the product name instead of
/// `oi`. Falls through to running in this process when the child cannot start.
#[cfg(unix)]
pub(crate) fn hand_off_under_product_name() {
  use std::os::unix::process::CommandExt as _;
  use std::process::Stdio;

  let Some(invoked) = std::env::args_os().next() else {
    return;
  };
  let Ok(exe) = std::env::current_exe() else {
    return;
  };
  let Ok(resolved) = std::fs::canonicalize(&exe) else {
    return;
  };
  if !should_relaunch_as_openit(Path::new(&invoked), &resolved) {
    return;
  }
  match std::process::Command::new(&resolved)
    .args(std::env::args_os().skip(1))
    .arg0(PRODUCT_BIN)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .process_group(0)
    .spawn()
  {
    Ok(_) => std::process::exit(0),
    Err(error) => tracing::error!(%error, "could not start OpenIt; running in this process"),
  }
}

fn starts_with_dash(arg: &OsStr) -> bool {
  arg.as_encoded_bytes().first() == Some(&b'-')
}

fn resolve_operand(arg: &OsStr, cwd: &Path, paths: &mut Vec<PathBuf>, errors: &mut Vec<String>) {
  let raw = PathBuf::from(arg);
  let joined = if raw.is_absolute() {
    raw.clone()
  } else {
    cwd.join(&raw)
  };
  let shown = raw.display();
  if !joined.exists() {
    errors.push(format!("openit: {shown}: no such file"));
    return;
  }
  if joined.is_dir() {
    errors.push(format!("openit: {shown}: is a directory"));
    return;
  }
  paths.push(std::path::absolute(&joined).unwrap_or(joined));
}

#[cfg(test)]
mod tests {
  use std::fs;

  use super::*;

  fn temp_cwd() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();
    (dir, cwd)
  }

  fn open_of(command: Command) -> ResolvedPaths {
    match command {
      Command::Open(resolved) => resolved,
      other => panic!("expected Open, got {other:?}"),
    }
  }

  fn abs(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap()
  }

  #[test]
  fn paths_become_absolute_and_keep_order() {
    let (_keep, cwd) = temp_cwd();
    let a = cwd.join("a.md");
    let b = cwd.join("b.txt");
    fs::write(&a, "a").unwrap();
    fs::write(&b, "b").unwrap();

    let resolved = open_of(parse(["a.md", "b.txt"], &cwd).unwrap());
    assert_eq!(resolved.paths, vec![abs(&a), abs(&b)]);
    assert!(resolved.errors.is_empty());
    assert!(resolved.had_operands);
  }

  #[test]
  fn double_dash_ends_flags() {
    let (_keep, cwd) = temp_cwd();
    let flagged = cwd.join("--help");
    fs::write(&flagged, "help file").unwrap();

    let resolved = open_of(parse(["--", "--help"], &cwd).unwrap());
    assert_eq!(resolved.paths, vec![abs(&flagged)]);
    assert!(resolved.errors.is_empty());
  }

  #[test]
  fn help_flag() {
    let (_keep, cwd) = temp_cwd();
    assert_eq!(parse(["--help"], &cwd), Ok(Command::Help));
  }

  #[test]
  fn version_flag() {
    let (_keep, cwd) = temp_cwd();
    assert_eq!(parse(["--version"], &cwd), Ok(Command::Version));
  }

  #[test]
  fn unknown_flag() {
    let (_keep, cwd) = temp_cwd();
    assert_eq!(parse(["--wat"], &cwd), Err(UnknownFlag("--wat".into())));
  }

  #[test]
  fn missing_file_is_dropped() {
    let (_keep, cwd) = temp_cwd();
    let resolved = open_of(parse(["missing.md"], &cwd).unwrap());
    assert!(resolved.paths.is_empty());
    assert_eq!(resolved.errors, ["openit: missing.md: no such file"]);
    assert!(resolved.is_empty_after_operands());
  }

  #[test]
  fn directory_is_dropped() {
    let (_keep, cwd) = temp_cwd();
    fs::create_dir(cwd.join("docs")).unwrap();
    let resolved = open_of(parse(["docs"], &cwd).unwrap());
    assert!(resolved.paths.is_empty());
    assert_eq!(resolved.errors, ["openit: docs: is a directory"]);
    assert!(resolved.is_empty_after_operands());
  }

  #[test]
  fn empty_argv_is_an_empty_launch() {
    let (_keep, cwd) = temp_cwd();
    let resolved = open_of(parse(std::iter::empty::<&str>(), &cwd).unwrap());
    assert!(resolved.paths.is_empty());
    assert!(resolved.errors.is_empty());
    assert!(!resolved.had_operands);
    assert!(!resolved.is_empty_after_operands());
  }

  #[test]
  fn relaunch_when_invoked_as_a_command_link() {
    let resolved = Path::new("/app/OpenIt");
    assert!(should_relaunch_as_openit(Path::new("/usr/local/bin/oi"), resolved));
    assert!(should_relaunch_as_openit(Path::new("/usr/local/bin/openit"), resolved));
    assert!(!should_relaunch_as_openit(Path::new("/app/OpenIt"), resolved));
    assert!(!should_relaunch_as_openit(
      Path::new("/usr/local/bin/oi"),
      Path::new("/usr/local/bin/oi")
    ));
  }
}
