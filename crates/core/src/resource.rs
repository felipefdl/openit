//! Where a document reference points and whether OpenIt may load it.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read as _};
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use url::{Host, Url};

use crate::cache::MAX_RESOURCE_BYTES;
use crate::error::Error;
use crate::settings::Settings;

/// Why a reference is refused before any policy is consulted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenyReason {
  /// A URL scheme other than http or https.
  Scheme(String),
  /// A URL carrying a user name or password.
  Credentials,
  /// A relative path in a document that has no location.
  NoLocalBase,
  /// A local path on a different device.
  OutsideDevice,
  /// Not parseable as a path or URL.
  Malformed,
}

impl fmt::Display for DenyReason {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Scheme(scheme) => write!(f, "{scheme}: links are not loaded"),
      Self::Credentials => f.write_str("URLs with credentials are not loaded"),
      Self::NoLocalBase => f.write_str("this document has no location for relative paths"),
      Self::OutsideDevice => f.write_str("the reference is on another device"),
      Self::Malformed => f.write_str("the reference is not a valid path or URL"),
    }
  }
}

/// A reference after resolution against its document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
  /// A file on this machine.
  Local(PathBuf),
  /// An http(s) URL without credentials.
  Remote(Url),
  /// Refused regardless of settings.
  Denied(DenyReason),
}

/// Resolve `reference` as written in a document whose directory is `base_dir`.
pub fn resolve(reference: &str, base_dir: Option<&Path>) -> Resolved {
  let trimmed = reference.trim();
  if trimmed.is_empty() {
    return Resolved::Denied(DenyReason::Malformed);
  }
  let is_drive_path = is_windows_drive_path(trimmed);
  #[cfg(windows)]
  if is_drive_path {
    return Resolved::Local(normalize(Path::new(trimmed)));
  }
  if !is_drive_path && let Ok(url) = Url::parse(trimmed) {
    return match url.scheme() {
      "http" | "https" => {
        if !url.username().is_empty() || url.password().is_some() {
          Resolved::Denied(DenyReason::Credentials)
        } else if url.host_str().is_none() {
          Resolved::Denied(DenyReason::Malformed)
        } else {
          Resolved::Remote(url)
        }
      },
      other => Resolved::Denied(DenyReason::Scheme(other.to_owned())),
    };
  }
  let decoded = percent_decode(trimmed);
  let path = Path::new(&decoded);
  if path.is_absolute() {
    return Resolved::Local(normalize(path));
  }
  base_dir.map_or(Resolved::Denied(DenyReason::NoLocalBase), |base| {
    Resolved::Local(normalize(&base.join(path)))
  })
}

fn is_windows_drive_path(reference: &str) -> bool {
  let mut bytes = reference.bytes();
  let (Some(drive), Some(colon), Some(separator)) = (bytes.next(), bytes.next(), bytes.next()) else {
    return false;
  };
  drive.is_ascii_alphabetic()
    && colon == b':'
    && matches!(separator, b'/' | b'\\')
    && !matches!(bytes.next(), Some(b'/' | b'\\'))
}

/// The registrable domain of a host, per the full Public Suffix List.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DomainFamilyKind {
  /// A registrable domain and all of its subdomains.
  Registrable,
  /// An IP literal, dotless host, or host with an unknown suffix.
  ExactHost,
}

/// A permission family identified by a normalized host name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DomainFamily {
  name: String,
  kind: DomainFamilyKind,
}

impl DomainFamily {
  /// Family of `host`, lowercased.
  pub fn of_host(host: &str) -> Self {
    let host = normalize_host(host);
    if host.parse::<IpAddr>().is_ok() || !host.contains('.') {
      return Self::exact(host);
    }
    let Some(suffix) = psl::suffix(host.as_bytes()) else {
      return Self::exact(host);
    };
    if suffix.typ().is_none() {
      return Self::exact(host);
    }
    let Some(domain) = psl::domain_str(&host) else {
      return Self::exact(host);
    };
    Self::registrable(domain.to_owned())
  }

  /// The kind of this permission family.
  pub const fn kind(&self) -> DomainFamilyKind {
    self.kind
  }

  const fn exact(name: String) -> Self {
    Self { name, kind: DomainFamilyKind::ExactHost }
  }

  const fn registrable(name: String) -> Self {
    Self {
      name,
      kind: DomainFamilyKind::Registrable,
    }
  }

  fn from_entry(entry: &str) -> Option<Self> {
    let entry = normalize_host(entry);
    if entry.is_empty() || Host::parse(&entry).is_err() {
      return None;
    }

    // Keep the explicit starter umbrella working even though it is a private
    // PSL suffix; other private suffixes remain subject to full-PSL rules.
    if entry == "githubusercontent.com" {
      return Some(Self::registrable(entry));
    }

    let suffix = psl::suffix(entry.as_bytes());
    let family = Self::of_host(&entry);
    if family.kind == DomainFamilyKind::Registrable {
      return (psl::domain_str(&entry) == Some(entry.as_str())).then_some(family);
    }
    if suffix.is_some_and(|suffix| suffix.typ().is_some()) {
      return None;
    }
    Some(family)
  }

  /// Whether `host` is this family or one of its subdomains.
  pub fn covers(&self, host: &str) -> bool {
    let host = normalize_host(host);
    match self.kind {
      DomainFamilyKind::ExactHost => host == self.name,
      DomainFamilyKind::Registrable => {
        host == self.name || host.strip_suffix(&self.name).is_some_and(|prefix| prefix.ends_with('.'))
      },
    }
  }
}

fn normalize_host(host: &str) -> String {
  host.trim_end_matches('.').to_ascii_lowercase()
}
impl fmt::Display for DomainFamily {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.name)
  }
}

/// What the policy says about a resolved reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
  /// Load it.
  Allow,
  /// Never load it; the reason is shown to the user.
  Deny(DenyReason),
  /// Ask the user about this family before loading.
  Ask(DomainFamily),
}

/// Apply `settings` to a resolved reference. Local files are always allowed
/// here; the caller still bounds the read.
pub fn decide(resolved: &Resolved, settings: &Settings) -> Decision {
  match resolved {
    Resolved::Local(_) => Decision::Allow,
    Resolved::Denied(reason) => Decision::Deny(reason.clone()),
    Resolved::Remote(url) => {
      let Some(host) = url.host_str() else {
        return Decision::Deny(DenyReason::Malformed);
      };
      if settings.allow_remote {
        return Decision::Allow;
      }
      let family = DomainFamily::of_host(host);
      let allowed = settings.allowed_domains.iter().any(|entry| {
        let Some(entry_family) = DomainFamily::from_entry(entry) else {
          tracing::warn!(entry, "ignoring invalid allowlist entry");
          return false;
        };
        entry_family.covers(host)
      });
      if allowed {
        Decision::Allow
      } else {
        Decision::Ask(family)
      }
    },
  }
}

/// Collapse `.` and `..` lexically; the file may not exist yet.
fn normalize(path: &Path) -> PathBuf {
  let mut out = PathBuf::new();
  for component in path.components() {
    match component {
      std::path::Component::CurDir => {},
      std::path::Component::ParentDir => {
        out.pop();
      },
      other => out.push(other.as_os_str()),
    }
  }
  out
}

fn percent_decode(s: &str) -> String {
  percent_encoding::percent_decode_str(s).decode_utf8_lossy().into_owned()
}

/// Read a local file after [`resolve`] returned [`Resolved::Local`].
///
/// The read is capped at [`MAX_RESOURCE_BYTES`] and refuses non-regular files
/// without blocking on their contents.
pub fn read_local(path: &Path) -> Result<Vec<u8>, Error> {
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
  let mut bytes = Vec::new();
  file
    .take(MAX_RESOURCE_BYTES.saturating_add(1))
    .read_to_end(&mut bytes)
    .map_err(|source| Error::Read { path: path_buf.clone(), source })?;
  let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
  if size > MAX_RESOURCE_BYTES {
    return Err(Error::TooLarge {
      path: path_buf,
      size,
      limit: MAX_RESOURCE_BYTES,
    });
  }
  Ok(bytes)
}

#[cfg(test)]
mod tests {
  use std::path::Path;

  use super::{Decision, DenyReason, DomainFamily, Resolved, decide, resolve};
  use crate::settings::Settings;

  fn remote(url: &str) -> Resolved {
    Resolved::Remote(url::Url::parse(url).unwrap())
  }

  #[test]
  fn relative_references_resolve_against_the_document_directory() {
    let base = Path::new("/docs/guide");
    assert_eq!(
      resolve("images/a.png", Some(base)),
      Resolved::Local("/docs/guide/images/a.png".into())
    );
    assert_eq!(resolve("./b.png", Some(base)), Resolved::Local("/docs/guide/b.png".into()));
    assert_eq!(resolve("../c.png", Some(base)), Resolved::Local("/docs/c.png".into()));
    assert_eq!(resolve("/abs/d.png", Some(base)), Resolved::Local("/abs/d.png".into()));
  }

  #[test]
  fn relative_references_without_a_base_are_denied() {
    assert_eq!(resolve("images/a.png", None), Resolved::Denied(DenyReason::NoLocalBase));
    #[cfg(unix)]
    assert_eq!(resolve("/abs/d.png", None), Resolved::Local("/abs/d.png".into()));
    #[cfg(windows)]
    assert_eq!(resolve("/abs/d.png", None), Resolved::Denied(DenyReason::NoLocalBase));
  }

  #[test]
  fn http_and_https_are_remote_and_everything_else_is_denied() {
    assert!(matches!(resolve("https://x.dev/a.png", None), Resolved::Remote(_)));
    assert!(matches!(resolve("http://x.dev/a.png", None), Resolved::Remote(_)));
    assert_eq!(
      resolve("file:///etc/passwd", None),
      Resolved::Denied(DenyReason::Scheme("file".into()))
    );
    assert_eq!(
      resolve("data:image/png;base64,AAAA", None),
      Resolved::Denied(DenyReason::Scheme("data".into()))
    );
    assert_eq!(
      resolve("javascript:alert(1)", None),
      Resolved::Denied(DenyReason::Scheme("javascript".into()))
    );
    assert_eq!(resolve("x://host/a", None), Resolved::Denied(DenyReason::Scheme("x".into())));
    assert_eq!(resolve("x:secret", None), Resolved::Denied(DenyReason::Scheme("x".into())));
  }

  #[test]
  fn embedded_credentials_are_denied() {
    assert_eq!(
      resolve("https://user:pw@x.dev/a.png", None),
      Resolved::Denied(DenyReason::Credentials)
    );
    assert_eq!(
      resolve("https://user@x.dev/a.png", None),
      Resolved::Denied(DenyReason::Credentials)
    );
  }

  #[test]
  fn windows_style_and_percent_encoded_local_paths_decode() {
    let base = Path::new("/docs");
    assert_eq!(
      resolve("img%20one.png", Some(base)),
      Resolved::Local("/docs/img one.png".into())
    );
    #[cfg(windows)]
    {
      assert_eq!(resolve("C:/img.png", None), Resolved::Local("C:/img.png".into()));
      assert_eq!(resolve(r"c:\img.png", None), Resolved::Local(r"c:\img.png".into()));
    }
    #[cfg(not(windows))]
    {
      assert_eq!(resolve("C:/img.png", None), Resolved::Denied(DenyReason::NoLocalBase));
      assert_eq!(resolve("C:/img.png", Some(base)), Resolved::Local("/docs/C:/img.png".into()));
      assert_eq!(resolve(r"c:\img.png", None), Resolved::Denied(DenyReason::NoLocalBase));
      assert_eq!(resolve(r"c:\img.png", Some(base)), Resolved::Local("/docs/c:\\img.png".into()));
    }
  }

  #[test]
  fn domain_family_is_the_registrable_domain() {
    assert_eq!(
      DomainFamily::of_host("raw.githubusercontent.com").to_string(),
      "raw.githubusercontent.com"
    );
    assert_eq!(DomainFamily::of_host("github.com").to_string(), "github.com");
    assert_eq!(DomainFamily::of_host("a.b.example.co.uk").to_string(), "example.co.uk");
    assert_eq!(DomainFamily::of_host("localhost").to_string(), "localhost");
    assert_eq!(DomainFamily::of_host("192.168.1.10").to_string(), "192.168.1.10");
    assert_eq!(DomainFamily::of_host("user.github.io").to_string(), "user.github.io");
    assert_eq!(DomainFamily::of_host("foo.appspot.com").to_string(), "foo.appspot.com");
    assert_ne!(
      DomainFamily::of_host("foo.appspot.com"),
      DomainFamily::of_host("evil.appspot.com")
    );
  }

  #[test]
  fn a_family_covers_itself_and_subdomains_only() {
    let f = DomainFamily::of_host("github.com");
    assert!(f.covers("github.com"));
    assert!(f.covers("api.github.com"));
    assert!(!f.covers("githubusercontent.com"));
    assert!(!f.covers("notgithub.com"));
  }

  #[test]
  fn exact_host_families_do_not_cover_subdomains() {
    for (family, subdomain) in [
      ("127.0.0.1", "evil.127.0.0.1"),
      ("localhost", "evil.localhost"),
      ("foo.invalid", "sub.foo.invalid"),
    ] {
      let family = DomainFamily::of_host(family);
      assert!(family.covers(family.to_string().as_str()));
      assert!(!family.covers(subdomain));
    }
  }

  #[test]
  fn decisions_follow_settings() {
    let mut settings = Settings::default();
    assert_eq!(
      decide(&remote("https://raw.githubusercontent.com/a.png"), &settings),
      Decision::Allow
    );
    assert_eq!(
      decide(&remote("https://example.org/a.png"), &settings),
      Decision::Ask(DomainFamily::of_host("example.org"))
    );
    settings.allowed_domains = vec!["EXAMPLE.ORG".to_owned()];
    assert_eq!(decide(&remote("https://img.example.org/a.png"), &settings), Decision::Allow);
    assert_eq!(decide(&Resolved::Local("/x".into()), &settings), Decision::Allow);
    assert_eq!(
      decide(&Resolved::Denied(DenyReason::Credentials), &settings),
      Decision::Deny(DenyReason::Credentials)
    );

    settings.allow_family(&DomainFamily::of_host("cdn.example.org"));
    assert_eq!(decide(&remote("https://img.example.org/a.png"), &settings), Decision::Allow);
    assert_eq!(settings.allowed_domains.iter().filter(|d| *d == "example.org").count(), 1);
    settings.allow_family(&DomainFamily::of_host("example.org"));
    assert_eq!(
      settings.allowed_domains.iter().filter(|d| *d == "example.org").count(),
      1,
      "no duplicates"
    );

    settings.allow_remote = true;
    assert_eq!(decide(&remote("https://anything.test/a.png"), &settings), Decision::Allow);
  }

  #[test]
  fn invalid_and_public_suffix_allowlist_entries_never_allow() {
    let target = remote("https://api.example.com/a.png");
    for entry in ["com", "co.uk", "github.io", "not a domain"] {
      let settings = Settings {
        allowed_domains: vec![entry.to_owned()],
        ..Settings::default()
      };
      assert!(matches!(decide(&target, &settings), Decision::Ask(_)), "{entry}");
    }

    let settings = Settings {
      allowed_domains: vec!["GitHub.COM.".to_owned()],
      ..Settings::default()
    };
    assert_eq!(decide(&remote("https://api.github.com/a.png"), &settings), Decision::Allow);
  }

  #[test]
  fn default_settings_carry_the_starter_domains_and_remote_off() {
    let s = Settings::default();
    assert!(!s.allow_remote);
    assert_eq!(
      s.allowed_domains,
      vec![
        "github.com".to_owned(),
        "githubusercontent.com".to_owned(),
        "schemastore.org".to_owned(),
      ]
    );
    assert_eq!(decide(&remote("https://www.schemastore.org/package.json"), &s), Decision::Allow);
  }

  #[test]
  fn settings_file_without_the_new_keys_still_gets_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    std::fs::write(&path, "autosave = true\n").unwrap();
    let s = Settings::load(&path).unwrap();
    assert!(s.autosave);
    assert_eq!(s.allowed_domains.len(), 3);
  }

  #[test]
  fn read_local_returns_regular_file_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("schema.json");
    std::fs::write(&path, b"{\"type\":\"string\"}").unwrap();
    assert_eq!(super::read_local(&path).unwrap(), b"{\"type\":\"string\"}");
  }
}
