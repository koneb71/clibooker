//! Environment variable names.
//!
//! Windows treats names case-insensitively (`Path` and `PATH` are one variable), so on Windows
//! `EnvKey` compares with ASCII case folded while keeping the spelling it was created with.
//! Elsewhere it is a plain byte-wise name.

use std::cmp::Ordering;
use std::ffi::{OsStr, OsString};
use std::fmt;

#[derive(Clone)]
pub struct EnvKey(OsString);

impl EnvKey {
    pub fn new(k: impl Into<OsString>) -> Self {
        EnvKey(k.into())
    }

    pub fn as_os_str(&self) -> &OsStr {
        &self.0
    }

    pub fn into_os_string(self) -> OsString {
        self.0
    }

    pub fn to_string_lossy(&self) -> std::borrow::Cow<'_, str> {
        self.0.to_string_lossy()
    }

    /// Case-insensitive on Windows, exact elsewhere.
    pub fn is(&self, name: &str) -> bool {
        same_name(self.0.as_encoded_bytes(), name.as_bytes())
    }

    /// Starts with `prefix` (case-insensitive on Windows).
    pub fn starts_with(&self, prefix: &str) -> bool {
        let b = self.0.as_encoded_bytes();
        b.len() >= prefix.len() && same_name(&b[..prefix.len()], prefix.as_bytes())
    }
}

fn same_name(a: &[u8], b: &[u8]) -> bool {
    if cfg!(windows) { a.eq_ignore_ascii_case(b) } else { a == b }
}

impl Ord for EnvKey {
    fn cmp(&self, other: &Self) -> Ordering {
        let (a, b) = (self.0.as_encoded_bytes(), other.0.as_encoded_bytes());
        if cfg!(windows) {
            a.iter().map(u8::to_ascii_uppercase).cmp(b.iter().map(u8::to_ascii_uppercase))
        } else {
            a.cmp(b)
        }
    }
}

impl PartialOrd for EnvKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for EnvKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for EnvKey {}

impl fmt::Debug for EnvKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl AsRef<OsStr> for EnvKey {
    fn as_ref(&self) -> &OsStr {
        &self.0
    }
}

impl From<&str> for EnvKey {
    fn from(s: &str) -> Self {
        EnvKey(s.into())
    }
}

impl From<String> for EnvKey {
    fn from(s: String) -> Self {
        EnvKey(s.into())
    }
}

impl From<&String> for EnvKey {
    fn from(s: &String) -> Self {
        EnvKey(s.into())
    }
}

impl From<OsString> for EnvKey {
    fn from(s: OsString) -> Self {
        EnvKey(s)
    }
}

impl From<&OsStr> for EnvKey {
    fn from(s: &OsStr) -> Self {
        EnvKey(s.to_os_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn folds_case_only_on_windows() {
        let mut m: BTreeMap<EnvKey, &str> = BTreeMap::new();
        m.insert("Path".into(), "a");
        m.insert("PATH".into(), "b");
        if cfg!(windows) {
            assert_eq!(m.len(), 1);
            // The first spelling is kept, the value is replaced.
            assert_eq!(m.keys().next().unwrap().as_os_str(), "Path");
            assert_eq!(m[&EnvKey::from("path")], "b");
        } else {
            assert_eq!(m.len(), 2);
        }
        assert!(EnvKey::from("BK_LAST").starts_with("BK_"));
        assert!(EnvKey::from("PATH").is("PATH"));
    }
}
