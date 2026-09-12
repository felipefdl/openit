//! User settings: one TOML file in the platform config directory.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::resource::DomainFamily;
use crate::save::write_atomic;

/// How theme appearance follows the operating system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
  /// Follow the operating system appearance.
  #[default]
  System,
  /// Always use the configured light theme.
  Light,
  /// Always use the configured dark theme.
  Dark,
}

/// Theme settings persisted in the user configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeSettings {
  /// Whether to follow the system or pin an appearance.
  pub mode: ThemeMode,
  /// Theme id used for light appearances.
  pub light: String,
  /// Theme id used for dark appearances.
  pub dark: String,
}

impl Default for ThemeSettings {
  fn default() -> Self {
    Self {
      mode: ThemeMode::System,
      light: "warm-burnout-light".to_owned(),
      dark: "warm-burnout-dark".to_owned(),
    }
  }
}

/// Preset widths for the centered Markdown preview column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkdownPreviewWidth {
  /// Use a readable 700 px column.
  #[default]
  Readable,
  /// Use a wider 960 px column.
  Wide,
  /// Use all available width while retaining minimum side padding.
  FullWidth,
}

/// The mode used when opening Markdown documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkdownMode {
  /// Open Markdown documents in preview mode.
  #[default]
  Preview,
  /// Open Markdown documents in edit mode.
  Edit,
}

/// Largest settings file accepted by the configuration loader.
pub const MAX_SETTINGS_BYTES: u64 = 1024 * 1024;

/// Settings the user can change. Unknown keys in the file are ignored so an
/// older build can read a newer file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
  /// Save documents with a path automatically after an editing pause.
  pub autosave: bool,
  /// Fetch remote images and schemas from any domain without asking.
  pub allow_remote: bool,
  /// Registrable domains whose subdomains may be fetched without asking.
  #[serde(default = "default_allowed_domains")]
  pub allowed_domains: Vec<String>,
  /// Theme appearance mode and selected light and dark theme ids.
  #[serde(default)]
  pub theme: ThemeSettings,
  /// Keep the status bar visible in Markdown preview, where it is hidden by default.
  #[serde(default)]
  pub always_show_status_bar: bool,
  /// Preset width used by the centered Markdown preview column.
  #[serde(default)]
  pub markdown_preview_width: MarkdownPreviewWidth,
  /// Mode used when opening Markdown documents.
  #[serde(default)]
  pub markdown_mode: MarkdownMode,
  /// Manual schema picks keyed by the document's canonical path.
  #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
  pub schemas: BTreeMap<String, String>,
}

fn default_allowed_domains() -> Vec<String> {
  vec![
    "github.com".to_owned(),
    "githubusercontent.com".to_owned(),
    "schemastore.org".to_owned(),
  ]
}

impl Default for Settings {
  fn default() -> Self {
    Self {
      autosave: false,
      allow_remote: false,
      allowed_domains: default_allowed_domains(),
      theme: ThemeSettings::default(),
      always_show_status_bar: false,
      markdown_preview_width: MarkdownPreviewWidth::default(),
      markdown_mode: MarkdownMode::default(),
      schemas: BTreeMap::new(),
    }
  }
}

impl Settings {
  /// The platform default: `<config_dir>/openit/settings.toml`.
  pub fn default_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("openit").join("settings.toml"))
  }

  /// Read `path`. A missing file yields the defaults.
  pub fn load(path: &Path) -> Result<Self, Error> {
    let path_buf = path.to_path_buf();
    let path_metadata = match fs::metadata(path) {
      Ok(metadata) => metadata,
      Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
      Err(source) => return Err(Error::Settings { path: path_buf, source }),
    };
    if !path_metadata.is_file() {
      return Err(Error::Settings {
        path: path_buf,
        source: io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"),
      });
    }
    let file = match File::open(path) {
      Ok(file) => file,
      Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
      Err(source) => return Err(Error::Settings { path: path_buf, source }),
    };
    let metadata = file
      .metadata()
      .map_err(|source| Error::Settings { path: path_buf.clone(), source })?;
    if !metadata.is_file() {
      return Err(Error::Settings {
        path: path_buf,
        source: io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"),
      });
    }
    let size = metadata.len();
    if size > MAX_SETTINGS_BYTES {
      return Err(Error::Settings {
        path: path_buf,
        source: io::Error::new(io::ErrorKind::InvalidData, "settings file exceeds size limit"),
      });
    }
    let capacity = usize::try_from(size.min(MAX_SETTINGS_BYTES)).unwrap_or(usize::MAX);
    let mut text = String::with_capacity(capacity);
    let bytes_read = file
      .take(MAX_SETTINGS_BYTES + 1)
      .read_to_string(&mut text)
      .map_err(|source| Error::Settings { path: path_buf.clone(), source })?;
    let bytes_read = u64::try_from(bytes_read).unwrap_or(u64::MAX);
    if bytes_read > MAX_SETTINGS_BYTES {
      return Err(Error::Settings {
        path: path_buf,
        source: io::Error::new(io::ErrorKind::InvalidData, "settings file exceeds size limit"),
      });
    }
    toml::from_str(&text).map_err(|source| Error::SettingsFormat { path: path_buf, source })
  }

  /// Write `self` to `path`, creating parent directories.
  pub fn save(&self, path: &Path) -> Result<(), Error> {
    let settings_err = |source: std::io::Error| Error::Settings { path: path.to_path_buf(), source };
    let dir = path
      .parent()
      .filter(|p| !p.as_os_str().is_empty())
      .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir).map_err(settings_err)?;
    let text = toml::to_string_pretty(self).map_err(|e| settings_err(std::io::Error::other(e)))?;
    write_atomic(path, dir, |_| Ok(()), |w| w.write_all(text.as_bytes())).map_err(settings_err)?;
    Ok(())
  }

  /// Persist permission for `family` (idempotent).
  pub fn allow_family(&mut self, family: &DomainFamily) {
    let name = family.to_string();
    if !self.allowed_domains.iter().any(|domain| domain == &name) {
      self.allowed_domains.push(name);
    }
  }
}

#[cfg(test)]
mod tests {
  use std::fs;

  use super::{Error, MAX_SETTINGS_BYTES, MarkdownMode, MarkdownPreviewWidth, Settings};

  #[test]
  fn missing_file_is_the_default() {
    let dir = tempfile::tempdir().unwrap();
    let settings = Settings::load(&dir.path().join("settings.toml")).unwrap();
    assert_eq!(settings, Settings::default());
    assert!(!settings.autosave);
  }

  #[test]
  fn autosave_reads_from_toml_and_unknown_keys_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    fs::write(&path, "autosave = true\nfuture_key = 1\n").unwrap();

    assert!(Settings::load(&path).unwrap().autosave);
  }

  #[test]
  fn the_status_bar_is_not_pinned_visible_by_default_and_reads_from_toml() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    assert!(!Settings::default().always_show_status_bar);

    fs::write(&path, "always_show_status_bar = true\n").unwrap();

    assert!(Settings::load(&path).unwrap().always_show_status_bar);
  }

  #[test]
  fn invalid_toml_is_an_error_naming_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    fs::write(&path, "autosave = [").unwrap();

    let err = Settings::load(&path).unwrap_err();
    assert!(err.to_string().contains("settings.toml"));
  }

  #[test]
  fn save_creates_parents_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("settings.toml");
    let settings = Settings { autosave: true, ..Settings::default() };

    settings.save(&path).unwrap();

    assert_eq!(Settings::load(&path).unwrap(), settings);
    assert!(fs::read_to_string(&path).unwrap().contains("autosave = true"));
  }

  #[test]
  fn markdown_preferences_save_and_reload() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    let settings = Settings {
      markdown_preview_width: MarkdownPreviewWidth::Wide,
      markdown_mode: MarkdownMode::Edit,
      ..Settings::default()
    };

    settings.save(&path).unwrap();

    assert_eq!(Settings::load(&path).unwrap(), settings);
  }

  #[test]
  fn schema_picks_save_and_reload_by_canonical_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    let mut settings = Settings::default();
    settings.schemas.insert(
      "/tmp/package.json".to_owned(),
      "https://www.schemastore.org/package.json".to_owned(),
    );

    settings.save(&path).unwrap();

    let loaded = Settings::load(&path).unwrap();
    assert_eq!(
      loaded.schemas.get("/tmp/package.json").map(String::as_str),
      Some("https://www.schemastore.org/package.json")
    );
    assert!(fs::read_to_string(&path).unwrap().contains("package.json"));
  }

  #[test]
  fn oversized_settings_file_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    let file = fs::File::create(&path).unwrap();
    file.set_len(MAX_SETTINGS_BYTES + 1).unwrap();

    let err = Settings::load(&path).unwrap_err();

    assert!(matches!(
      err,
      Error::Settings { source, .. } if source.kind() == std::io::ErrorKind::InvalidData
    ));
  }

  #[cfg(unix)]
  #[test]
  fn a_fifo_at_the_settings_path_is_refused_without_blocking() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    assert!(std::process::Command::new("mkfifo").arg(&path).status().unwrap().success());
    let load_path = path;
    let (sender, receiver) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || sender.send(Settings::load(&load_path)).unwrap());

    let err = receiver.recv_timeout(std::time::Duration::from_secs(2)).unwrap().unwrap_err();
    handle.join().unwrap();

    assert!(matches!(
      err,
      Error::Settings { source, .. } if source.kind() == std::io::ErrorKind::InvalidInput
    ));
  }
}
