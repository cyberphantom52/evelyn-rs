//! Identifying an image file from its magic bytes.

use crate::Error;
use android_firmware::Sha256Digest;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageFormat {
    Erofs,
    Ext4,
    AndroidSparse,
    Unknown,
}

impl ImageFormat {
    /// Identify a format from the start of an image. A prefix too short to hold a
    /// format's magic does not match it.
    pub(crate) fn of(prefix: &[u8]) -> Self {
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
        let prefix = File::open(path)
            .and_then(|mut file| header(&mut file))
            .map_err(|source| Error::Read {
                path: path.into(),
                source,
            })?;
        Ok(Self::of(&prefix))
    }
}

/// The start of an image, as much of it as holds every format's magic.
fn header(file: &mut File) -> io::Result<Vec<u8>> {
    let mut prefix = Vec::with_capacity(2048);
    file.take(2048).read_to_end(&mut prefix)?;
    Ok(prefix)
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
            let prefix = header(&mut file)?;
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
