//! Firmware identities, Android metadata, and validated paths.

use android_firmware::{AndroidMetadata, Name, RelativePath, Sha256Digest, SourceRef};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid {kind}: {value:?}")]
    Invalid { kind: &'static str, value: String },
    #[error(transparent)]
    Firmware(#[from] android_firmware::Error),
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
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
