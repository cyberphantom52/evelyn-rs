//! Identifying an image file from its magic bytes.

use crate::Error;
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    fs::File,
    io::{self, Read},
    path::Path,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Erofs,
    Ext4,
    AndroidSparse,
    /// A boot or vendor_boot image, whose ramdisk is read as a partition.
    Boot,
    Unknown,
}

impl ImageFormat {
    /// Identify a format from the start of an image. A prefix too short to hold a
    /// format's magic does not match it.
    pub(crate) fn of(prefix: &[u8]) -> Self {
        let magic = |at: usize, bytes: &[u8]| prefix.get(at..at + bytes.len()) == Some(bytes);
        if magic(0, &[0x3a, 0xff, 0x26, 0xed]) {
            Self::AndroidSparse
        } else if magic(0, b"ANDROID!") || magic(0, b"VNDRBOOT") {
            Self::Boot
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Filesystem {
    Erofs,
    Ext4,
    /// A boot image's cpio ramdisk.
    Ramdisk,
}

impl fmt::Display for Filesystem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Erofs => "erofs",
            Self::Ext4 => "ext4",
            Self::Ramdisk => "ramdisk",
        })
    }
}
