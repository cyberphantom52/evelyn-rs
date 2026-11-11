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

use std::{io, path::PathBuf};
use thiserror::Error;

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
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid {kind}: {value:?}")]
    Invalid { kind: &'static str, value: String },
    #[error("ext4 backend: {0}")]
    Ext4(#[from] fs_ext4::Error),
    #[error("filesystem IO: {0}")]
    Io(#[from] io::Error),
    #[error("unsupported or inconsistent image data: {0}")]
    Unsupported(String),
    #[error("cannot build super image: {0}")]
    Super(String),
    #[error("EROFS backend: {0}")]
    Erofs(#[from] fs_erofs::Error),
}
