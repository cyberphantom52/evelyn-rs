//! Firmware identities, Android metadata, and validated paths.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid {kind}: {value:?}")]
    Invalid { kind: &'static str, value: String },
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
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

/// Where a file's bytes are: unchanged in a source image, or in a tree's own store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Content {
    /// Inode `inode` of source image `source`.
    Image { source: SourceRef, inode: u64 },
    /// A file in the tree's `content/` directory, written by an edit.
    Blob(Name),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum EntryKind {
    File {
        content: Content,
        sha256: Sha256Digest,
        size: u64,
    },
    Directory,
    Symlink {
        target: Vec<u8>,
    },
    Hardlink {
        target: RelativePath,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub kind: EntryKind,
    pub metadata: AndroidMetadata,
    pub provenance: Vec<Provenance>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Provenance {
    Image {
        source: Option<Name>,
        partition: Name,
        path: RelativePath,
        sha256: Sha256Digest,
    },
    Asset {
        path: RelativePath,
        sha256: Sha256Digest,
    },
    Step {
        id: Name,
    },
}

mod tree;
pub use tree::PartitionTree;
mod hashing;
pub use hashing::Sha256Writer;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Filesystem {
    Erofs,
    Ext4,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Output {
    Partitions,
    Super,
    Zip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageFormat {
    Erofs,
    Ext4,
    AndroidSparse,
    Unknown,
}

impl ImageFormat {
    /// Identify a format from an image's first 2 KiB.
    fn of(prefix: &[u8]) -> Self {
        let magic = |at: usize, bytes: &[u8]| prefix.get(at..at + bytes.len()) == Some(bytes);
        if magic(0, &[0x3a, 0xff, 0x26, 0xed]) {
            Self::AndroidSparse
        } else if magic(1024, &[0xe2, 0xe1, 0xf5, 0xe0]) {
            Self::Erofs
        } else if magic(1080, &[0x53, 0xef]) {
            Self::Ext4
        } else {
            Self::Unknown
        }
    }

    /// Identify an image's format without reading past its header.
    pub fn detect(path: &Path) -> Result<Self, Error> {
        let mut prefix = Vec::with_capacity(2048);
        File::open(path)
            .and_then(|file| file.take(2048).read_to_end(&mut prefix))
            .map_err(|source| Error::Read {
                path: path.into(),
                source,
            })?;
        Ok(Self::of(&prefix))
    }
}

#[derive(Debug, Serialize)]
pub struct Artifact {
    pub path: PathBuf,
    pub size: u64,
    pub sha256: Sha256Digest,
    pub format: ImageFormat,
}

impl Artifact {
    /// Hash a file and identify its format from magic bytes.
    pub fn inspect(path: &Path) -> Result<Self, Error> {
        let inspect = || -> io::Result<(u64, ImageFormat, Sha256Digest)> {
            let mut file = File::open(path)?;
            let metadata = file.metadata()?;
            if !metadata.is_file() {
                return Err(io::Error::other("expected a regular file"));
            }
            let mut prefix = Vec::with_capacity(2048);
            (&mut file).take(2048).read_to_end(&mut prefix)?;
            let format = ImageFormat::of(&prefix);
            let digest = Sha256Digest::reader(prefix.as_slice().chain(file))?;
            Ok((metadata.len(), format, digest))
        };
        let (size, format, sha256) = inspect().map_err(|source| Error::Read {
            path: path.into(),
            source,
        })?;
        Ok(Self {
            path: path.into(),
            size,
            sha256,
            format,
        })
    }
}
