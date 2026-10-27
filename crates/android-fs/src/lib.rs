//! Android filesystems: EROFS and ext4 images (raw or sparse) read in place, scanned into
//! partition trees, and written back out.

pub mod device;
pub mod erofs;
pub mod image;

use std::io;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Model(#[from] model::Error),
    #[error("ext4 backend: {0}")]
    Ext4(#[from] fs_ext4::Error),
    #[error("filesystem IO: {0}")]
    Io(#[from] io::Error),
    #[error("unsupported or inconsistent image data: {0}")]
    Unsupported(String),
}
