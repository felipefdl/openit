//! Errors the document library reports. Every variant names the path so the
//! app can show which file failed without reformatting the message.

use std::path::PathBuf;

/// Document library failure.
#[derive(Debug, thiserror::Error)]
pub enum Error {
  /// The file could not be read.
  #[error("could not read {path}: {source}")]
  Read {
    /// File that failed.
    path: PathBuf,
    /// Underlying I/O error.
    #[source]
    source: std::io::Error,
  },
  /// The file is over the size cap for its reader.
  #[error("{path} is {size} bytes, over the {limit} byte limit")]
  TooLarge {
    /// File that failed.
    path: PathBuf,
    /// Size on disk.
    size: u64,
    /// Cap the reader enforces.
    limit: u64,
  },
  /// The bytes are not valid UTF-8.
  #[error("{path} is not UTF-8 text")]
  NotUtf8 {
    /// File that failed.
    path: PathBuf,
  },
  /// The kind has no text reader.
  #[error("{path} is not a text document")]
  Unsupported {
    /// File that failed.
    path: PathBuf,
  },
  /// The bytes are not a readable image of a supported format.
  #[error("could not read {path} as an image: {reason}")]
  Decode {
    /// Path that failed.
    path: PathBuf,
    /// Decoder message.
    reason: String,
  },
  /// The requested pixel operation is not available for this format.
  #[error("{reason}")]
  Format {
    /// What could not be done.
    reason: String,
  },
  /// The PDF engine refused the document or failed on its content.
  #[error("{reason}")]
  Pdf {
    /// What went wrong, in words the window can show.
    reason: String,
  },
  /// The file could not be written.
  #[error("could not write {path}: {source}")]
  Write {
    /// File that failed.
    path: PathBuf,
    /// Underlying I/O error.
    #[source]
    source: std::io::Error,
  },
  /// The resource cache could not be read or written.
  #[error("could not access cache {path}: {source}")]
  Cache {
    /// Cache file or directory.
    path: PathBuf,
    /// Underlying I/O error.
    #[source]
    source: std::io::Error,
  },
  /// The recovery store could not read or write a draft file.
  #[error("could not access draft {path}: {source}")]
  Recovery {
    /// Draft file or directory that failed.
    path: PathBuf,
    /// Underlying I/O error.
    #[source]
    source: std::io::Error,
  },
  /// A draft file is not valid JSON for the current format.
  #[error("draft {path} is not readable: {source}")]
  DraftFormat {
    /// Draft file that failed.
    path: PathBuf,
    /// Underlying parse error.
    #[source]
    source: serde_json::Error,
  },
  /// The settings file could not be read or written.
  #[error("could not access settings {path}: {source}")]
  Settings {
    /// Settings file.
    path: PathBuf,
    /// Underlying I/O error.
    #[source]
    source: std::io::Error,
  },
  /// The settings file is not valid TOML for the current schema.
  #[error("settings {path} is not readable: {source}")]
  SettingsFormat {
    /// Settings file.
    path: PathBuf,
    /// Underlying parse error.
    #[source]
    source: toml::de::Error,
  },
  /// A theme family is not valid.
  #[error("theme {name} is not readable: {reason}")]
  Theme {
    /// Family name or a generic theme label when parsing fails before its name is known.
    name: String,
    /// Human-readable parse or validation reason.
    reason: String,
  },
  /// The file watcher could not be started.
  #[error("could not watch {path}: {source}")]
  Watch {
    /// File that failed.
    path: PathBuf,
    /// Underlying watcher error.
    #[source]
    source: notify::Error,
  },
  /// The directory could not be listed.
  #[error("could not list {path}: {source}")]
  Browse {
    /// Directory that failed.
    path: PathBuf,
    /// Underlying I/O error.
    #[source]
    source: std::io::Error,
  },
  /// The local socket could not be named, bound, or accepted.
  #[error("could not listen on {path}: {source}")]
  Ipc {
    /// Socket path or named pipe that failed.
    path: PathBuf,
    /// Underlying I/O or protocol error.
    #[source]
    source: std::io::Error,
  },
}
