//! Names and paths: a partition-relative path, a path as the device sees it, and
//! the globs both can hold.

use crate::Error;
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    path::{Path, PathBuf},
};

macro_rules! string_type {
    ($name:ident, $kind:literal, $valid:expr) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);
        impl TryFrom<String> for $name {
            type Error = Error;
            fn try_from(value: String) -> Result<Self, Error> {
                if !($valid)(&value) {
                    return Err(Error::Invalid { kind: $kind, value });
                }
                Ok(Self(value))
            }
        }
        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

// An empty path is one empty component, so it is rejected too.
string_type!(RelativePath, "partition-relative path", |s: &str| !s
    .contains('\0')
    && s.split('/')
        .all(|part| !part.is_empty() && part != "." && part != ".."));
// A path as the device sees it (`/vendor/etc/fstab.qcom`); components may be globs.
string_type!(DevicePath, "device path", |s: &str| s.len() > 1
    && s.starts_with('/')
    && !s.contains('\0')
    && s[1..]
        .split('/')
        .all(|part| !part.is_empty() && part != "." && part != ".."));

impl DevicePath {
    /// The path's components, after the leading `/`.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0[1..].split('/')
    }
}

impl RelativePath {
    /// Join onto `root`. Validation already rules out absolute and `..` components.
    pub fn resolve(&self, root: &Path) -> PathBuf {
        root.join(&self.0)
    }
}

/// `*` matches any run of characters and `?` any one.
pub fn glob(pattern: &str, name: &str) -> bool {
    match pattern.chars().next() {
        None => name.is_empty(),
        Some('*') => (0..=name.len())
            .filter(|at| name.is_char_boundary(*at))
            .any(|at| glob(&pattern[1..], &name[at..])),
        Some(c) => name.chars().next().is_some_and(|n| {
            (c == '?' || c == n) && glob(&pattern[c.len_utf8()..], &name[n.len_utf8()..])
        }),
    }
}
