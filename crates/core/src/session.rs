//! Stable identity for one document session. It survives renames and Save As
//! and keys the session's draft in the recovery store.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Identity of one open document, independent of its path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(Uuid);

impl SessionId {
  /// A fresh random identity.
  pub fn new() -> Self {
    Self(Uuid::new_v4())
  }
}

impl Default for SessionId {
  fn default() -> Self {
    Self::new()
  }
}

impl fmt::Display for SessionId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}", self.0.as_hyphenated())
  }
}

impl FromStr for SessionId {
  type Err = uuid::Error;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    Uuid::parse_str(s).map(Self)
  }
}

#[cfg(test)]
mod tests {
  use super::SessionId;

  #[test]
  fn ids_are_unique_and_round_trip_as_text() {
    let a = SessionId::new();
    let b = SessionId::new();
    assert_ne!(a, b);

    let text = a.to_string();
    let parsed: SessionId = text.parse().unwrap();
    assert_eq!(parsed, a);
  }

  #[test]
  fn ids_round_trip_through_json() {
    let id = SessionId::new();
    let json = serde_json::to_string(&id).unwrap();
    let back: SessionId = serde_json::from_str(&json).unwrap();
    assert_eq!(back, id);
  }

  #[test]
  fn garbage_does_not_parse() {
    assert!("not-a-uuid".parse::<SessionId>().is_err());
  }
}
