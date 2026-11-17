//! What is true of Android, whatever ports it: EROFS and ext4 images (raw or sparse)
//! read in place, scanned into partition trees and written back out, alone or into a
//! `super` image; partitions, their paths and mount points; the metadata Android gives
//! a file and its defaults; property files; where the SELinux policy is; and hashes.

pub mod build;
pub mod defaults;
mod device;
mod format;
pub mod hash;
mod image;
pub mod lp;
pub mod metadata;
pub mod partition;
pub mod path;
pub mod property;
mod ramdisk;
mod region;
mod scan;
pub mod sepolicy;
mod store;
mod tree;

/// The file-type bits of an inode's mode.
pub(crate) mod mode {
    pub const TYPE: u16 = 0o170000;
    pub const FILE: u16 = 0o100000;
    pub const DIRECTORY: u16 = 0o040000;
    pub const SYMLINK: u16 = 0o120000;
}

use std::{
    io,
    path::{Path, PathBuf},
};
use thiserror::Error;

/// An I/O result with what it was about attached to its error: a file on the
/// host, or an entry of a partition.
pub(crate) trait IoAt<T> {
    fn at(self, path: impl AsRef<Path>) -> Result<T, Error>;
    fn entry(self, path: impl ToString) -> Result<T, Error>;
}

impl<T> IoAt<T> for io::Result<T> {
    fn at(self, path: impl AsRef<Path>) -> Result<T, Error> {
        self.map_err(|source| Error::Io {
            path: path.as_ref().to_path_buf(),
            source,
        })
    }

    fn entry(self, path: impl ToString) -> Result<T, Error> {
        self.map_err(|source| Error::Entry {
            path: path.to_string(),
            source,
        })
    }
}

pub use format::{Filesystem, ImageFormat};
pub use image::Image;
pub use region::Region;
pub use scan::scan;
pub use store::Store;
pub use tree::{Content, Entry, EntryKind, File, PartitionTree};

#[derive(Debug, Error)]
pub enum Error {
    #[error("no entry {0}")]
    Missing(crate::path::RelativePath),
    #[error("{0} is not a regular file")]
    NotAFile(crate::path::RelativePath),
    #[error("cannot read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid {kind}: {value:?}")]
    Invalid { kind: &'static str, value: String },
    #[error("ext4 filesystem")]
    Ext4(#[from] fs_ext4::Error),
    #[error("cannot access {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot read {path}")]
    Entry {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("cannot write the image")]
    Write(#[source] io::Error),
    #[error("cannot build {partition}")]
    Build {
        partition: String,
        #[source]
        source: Box<Error>,
    },
    #[error("unsupported or inconsistent image data: {0}")]
    Unsupported(String),
    #[error("cannot build super image: {0}")]
    Super(String),
    #[error("EROFS filesystem")]
    Erofs(#[from] fs_erofs::Error),
    #[error("boot image")]
    Boot(#[from] bootimg::Error),
}
