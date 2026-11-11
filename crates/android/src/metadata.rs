//! The metadata Android gives a file: its owner, group, mode and xattrs, the
//! SELinux label among them.

use crate::Error;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AndroidMetadata {
    pub uid: u32,
    pub gid: u32,
    pub mode: u16,
    /// Namespace-qualified xattr names and their raw values.
    pub xattrs: BTreeMap<String, Vec<u8>>,
}

/// The xattr an entry's SELinux label is in.
pub const SELINUX_XATTR: &str = "security.selinux";

impl AndroidMetadata {
    /// Label the entry `label`, and whether that changed it. A label is the same
    /// with or without its terminating NUL, as for the kernel; one written gets it.
    pub fn set_selinux(&mut self, label: &str) -> bool {
        let current = self.xattrs.get(SELINUX_XATTR);
        if current.is_some_and(|current| {
            current.strip_suffix(&[0]).unwrap_or(current) == label.as_bytes()
        }) {
            return false;
        }
        let mut value = label.as_bytes().to_vec();
        value.push(0);
        self.xattrs.insert(SELINUX_XATTR.to_owned(), value);
        true
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self.mode > 0o7777
            || self
                .xattrs
                .keys()
                .any(|key| key.is_empty() || key.len() > 255 || key.contains('\0'))
        {
            return Err(Error::Invalid {
                kind: "Android metadata",
                value: format!("invalid mode {:#o} or xattr name", self.mode),
            });
        }
        Ok(())
    }
}
