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

/// Whether `text` is a non-empty run of `[A-Za-z0-9._-]`: safe in a path component
/// or a recipe, for a value read from firmware.
pub fn is_token(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// Bytes as lowercase hex, two digits each.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The bytes `text` writes as hex, two digits each, or `None` if it is not that.
pub fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).ok())
        .collect()
}

/// A SHA-256 digest, written as 64 lowercase hex digits.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Sha256Digest([u8; 32]);

impl TryFrom<String> for Sha256Digest {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Error> {
        unhex(&value)
            .filter(|_| !value.bytes().any(|b| b.is_ascii_uppercase()))
            .and_then(|bytes| bytes.try_into().ok())
            .map(Self)
            .ok_or(Error::Invalid {
                kind: "SHA-256 digest",
                value,
            })
    }
}

impl From<Sha256Digest> for String {
    fn from(value: Sha256Digest) -> Self {
        hex(&value.0)
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex(&self.0))
    }
}

impl fmt::Debug for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sha256Digest({self})")
    }
}

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

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    fn finish(digest: Sha256) -> Self {
        Self(digest.finalize().into())
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

/// The two firmwares a port is built from: the ROM being ported, which supplies the
/// system side, and the device's stock firmware, which supplies the vendor side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Firmware {
    Rom,
    Stock,
}

impl fmt::Display for Firmware {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Rom => "rom",
            Self::Stock => "stock",
        })
    }
}

/// A partition image of one of the port's firmwares, written `firmware.partition`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRef {
    pub source: Firmware,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Filesystem {
    Erofs,
    Ext4,
}

impl fmt::Display for Filesystem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Erofs => "erofs",
            Self::Ext4 => "ext4",
        })
    }
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

/// Where the vendor names the platform policy version its own policy is written against.
pub const SEPOLICY_VERSION: &str = "/vendor/etc/selinux/plat_sepolicy_vers.txt";

/// The CIL files init compiles a device's split SELinux policy from, in the order it
/// passes them to secilc, for the platform policy `version` the vendor's targets. Init
/// requires the platform policy and its mapping, `plat_pub_versioned.cil` and the
/// vendor's policy, and uses each of the others only if the device has it.
pub fn sepolicy_files(version: &str) -> Result<Vec<DevicePath>, Error> {
    if !is_token(version) {
        return Err(Error::Invalid {
            kind: "SELinux policy version",
            value: version.to_owned(),
        });
    }
    [
        "/system/etc/selinux/plat_sepolicy.cil".to_owned(),
        format!("/system/etc/selinux/mapping/{version}.cil"),
        format!("/system/etc/selinux/mapping/{version}.compat.cil"),
        "/system_ext/etc/selinux/system_ext_sepolicy.cil".to_owned(),
        format!("/system_ext/etc/selinux/mapping/{version}.cil"),
        format!("/system_ext/etc/selinux/mapping/{version}.compat.cil"),
        "/product/etc/selinux/product_sepolicy.cil".to_owned(),
        format!("/product/etc/selinux/mapping/{version}.cil"),
        "/vendor/etc/selinux/plat_pub_versioned.cil".to_owned(),
        "/vendor/etc/selinux/vendor_sepolicy.cil".to_owned(),
        "/odm/etc/selinux/odm_sepolicy.cil".to_owned(),
    ]
    .into_iter()
    .map(DevicePath::try_from)
    .collect()
}

/// The partitions whose `etc/fs_config_files` and `etc/fs_config_dirs` Android reads,
/// in the order it reads them.
pub const FS_CONFIG: [&str; 6] = [
    "/system",
    "/vendor",
    "/oem",
    "/odm",
    "/product",
    "/system_ext",
];

/// The `file_contexts` files, in the order Android loads them.
pub const FILE_CONTEXTS: [&str; 5] = [
    "/system/etc/selinux/plat_file_contexts",
    "/system_ext/etc/selinux/system_ext_file_contexts",
    "/product/etc/selinux/product_file_contexts",
    "/vendor/etc/selinux/vendor_file_contexts",
    "/odm/etc/selinux/odm_file_contexts",
];
