//! Android firmware facts and addressing: partition names and paths, the device paths
//! partitions are mounted under, which side of Treble a partition is on, the `super`
//! layout, the metadata Android gives a file, and content hashes.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    io::{self, Read},
    path::{Path, PathBuf},
};
use thiserror::Error;

mod hashing;
pub use hashing::Sha256Writer;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid {kind}: {value:?}")]
    Invalid { kind: &'static str, value: String },
}

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

string_type!(Name, "name", |s: &str| s
    .bytes()
    .next()
    .is_some_and(|b| b.is_ascii_alphanumeric())
    && s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)));
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
string_type!(Sha256Digest, "SHA-256 digest", |s: &str| s.len() == 64
    && s.bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));

impl DevicePath {
    /// The path's components, after the leading `/`.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0[1..].split('/')
    }
}

/// Where each partition is mounted on the device, for resolving device paths.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Mounts(Vec<(String, Name)>);

impl Mounts {
    /// Mount points are absolute, like `/` or `/vendor`.
    pub fn new(mounts: impl IntoIterator<Item = (String, Name)>) -> Self {
        Self(mounts.into_iter().collect())
    }

    /// The partitions and partition-relative paths `path` refers to. A literal path
    /// belongs to the partition with the longest matching mount point; where globs
    /// in its leading components match other mount points, those partitions too.
    pub fn resolve(&self, path: &DevicePath) -> Vec<TargetPath> {
        let components: Vec<&str> = path.components().collect();
        let mut literal: Option<(usize, TargetPath)> = None;
        let mut globbed = Vec::new();
        for (mount, partition) in &self.0 {
            let mount: Vec<&str> = mount.split('/').filter(|c| !c.is_empty()).collect();
            if mount.len() >= components.len()
                || !mount.iter().zip(&components).all(|(m, c)| glob(c, m))
            {
                continue;
            }
            let Ok(rest) = RelativePath::try_from(components[mount.len()..].join("/")) else {
                continue;
            };
            let target = TargetPath {
                partition: partition.clone(),
                path: rest,
            };
            if components[..mount.len()]
                .iter()
                .any(|c| c.contains(['*', '?']))
            {
                globbed.push(target);
            } else if literal
                .as_ref()
                .is_none_or(|(depth, _)| mount.len() > *depth)
            {
                literal = Some((mount.len(), target));
            }
        }
        literal
            .map(|(_, target)| target)
            .into_iter()
            .chain(globbed)
            .collect()
    }

    /// The directory a partition provides on the device: its mount point, or for
    /// the partition mounted at `/` (a system-as-root `system`), `/<name>`.
    pub fn directory(&self, partition: &Name) -> Option<String> {
        let (mount, _) = self.0.iter().find(|(_, p)| p == partition)?;
        Some(if mount == "/" {
            format!("/{partition}")
        } else {
            mount.clone()
        })
    }

    /// The partition mounted at exactly `path`, if one is.
    pub fn partition_at(&self, path: &DevicePath) -> Option<&Name> {
        let path = path.to_string();
        self.0
            .iter()
            .find(|(mount, _)| mount.trim_end_matches('/') == path.trim_end_matches('/'))
            .map(|(_, partition)| partition)
    }

    /// The device path of `path` in `partition`, if the partition is mounted.
    pub fn device_path(&self, target: &TargetPath) -> Option<DevicePath> {
        let (mount, _) = self.0.iter().find(|(_, p)| *p == target.partition)?;
        DevicePath::try_from(format!("{}/{}", mount.trim_end_matches('/'), target.path)).ok()
    }
}

/// `*` matches any run of characters and `?` any one.
pub fn glob(pattern: &str, name: &str) -> bool {
    fn matches(pattern: &[u8], name: &[u8]) -> bool {
        match pattern.split_first() {
            None => name.is_empty(),
            Some((b'*', rest)) => (0..=name.len()).any(|skip| matches(rest, &name[skip..])),
            Some((b'?', rest)) => !name.is_empty() && matches(rest, &name[1..]),
            Some((byte, rest)) => name.first() == Some(byte) && matches(rest, &name[1..]),
        }
    }
    matches(pattern.as_bytes(), name.as_bytes())
}

impl RelativePath {
    /// Join onto `root`. Validation already rules out absolute and `..` components.
    pub fn resolve(&self, root: &Path) -> PathBuf {
        root.join(&self.0)
    }
}

impl Sha256Digest {
    pub fn bytes(bytes: &[u8]) -> Self {
        Self::finish(Sha256::new_with_prefix(bytes))
    }

    pub fn reader(mut reader: impl Read) -> io::Result<Self> {
        let mut writer = Sha256Writer::new(io::sink());
        io::copy(&mut reader, &mut writer)?;
        Ok(writer.finish()?.1)
    }

    fn finish(digest: Sha256) -> Self {
        Self(
            digest
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        )
    }
}

/// A path in a target partition, written `partition:path`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TargetPath {
    pub partition: Name,
    pub path: RelativePath,
}

impl TryFrom<String> for TargetPath {
    type Error = Error;

    fn try_from(value: String) -> Result<Self, Error> {
        let Some((partition, path)) = value.split_once(':') else {
            return Err(Error::Invalid {
                kind: "partition:path",
                value,
            });
        };
        Ok(Self {
            partition: partition.to_owned().try_into()?,
            path: path.to_owned().try_into()?,
        })
    }
}

impl From<TargetPath> for String {
    fn from(value: TargetPath) -> Self {
        format!("{}:{}", value.partition, value.path)
    }
}

impl fmt::Display for TargetPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.partition, self.path)
    }
}

/// A partition image of a named source firmware, written `source.partition`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRef {
    pub source: Name,
    pub partition: Name,
}

impl fmt::Display for SourceRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.source, self.partition)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AndroidMetadata {
    pub uid: u32,
    pub gid: u32,
    pub mode: u16,
    /// Namespace-qualified xattr names and their raw values.
    pub xattrs: std::collections::BTreeMap<String, Vec<u8>>,
}

impl AndroidMetadata {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Filesystem {
    Erofs,
    Ext4,
}

/// Treble's vendor side, in the order partitions are laid out in `super`. A device
/// lists the ones it has; recipes may name any of them, for conditions.
pub const VENDOR_SIDE: [&str; 4] = ["odm", "vendor", "vendor_dlkm", "odm_dlkm"];
/// System-side partitions every port takes from the ROM, in `super` order.
pub const STANDARD_SYSTEM: [&str; 4] = ["system", "system_ext", "product", "system_dlkm"];

/// The mount point a partition gets unless the port overrides it: `/<name>`, except
/// `system`, which depends on the image (see `system_mount_point`).
pub fn default_mount_point(partition: &str) -> Option<String> {
    (partition != "system").then(|| format!("/{partition}"))
}

/// `/` for a system-as-root `system` image, whose top level is the device's root
/// with `/system` beneath it, or `/system` for an older one, whose top level is
/// `/system` itself.
pub fn system_mount_point(has: impl Fn(&str) -> bool) -> Option<&'static str> {
    if has("system/build.prop") {
        Some("/")
    } else if has("build.prop") {
        Some("/system")
    } else {
        None
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuperLayout {
    pub size: u64,
    pub metadata_size: u32,
    pub slots: SlotScheme,
    /// The one dynamic partition group, named as the vendor names it.
    pub group: Name,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotScheme {
    Single,
    Ab,
    VirtualAb,
}

impl SuperLayout {
    pub fn metadata_slots(&self) -> u32 {
        match self.slots {
            SlotScheme::Single => 2,
            SlotScheme::Ab | SlotScheme::VirtualAb => 3,
        }
    }

    /// A slot's group may use the whole device, except on A/B, where both slots
    /// hold full copies.
    pub fn group_capacity(&self) -> u64 {
        match self.slots {
            SlotScheme::Ab => self.size / 2,
            SlotScheme::Single | SlotScheme::VirtualAb => self.size,
        }
    }
}
