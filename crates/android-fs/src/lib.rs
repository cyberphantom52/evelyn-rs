//! Android filesystems: EROFS and ext4 images (raw or sparse) read in place, scanned into
//! partition trees, and written back out.

pub mod build;
pub mod defaults;
mod device;
mod format;
mod image;
mod scan;
mod store;
mod tree;

use std::{
    fs, io,
    path::{Path, PathBuf},
};
use thiserror::Error;

pub use format::{Artifact, ImageFormat};
pub use image::Image;
pub use scan::scan;
pub use store::Store;
pub use tree::{Content, Entry, EntryKind, PartitionTree, Provenance};

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid partition tree: {0}")]
    InvalidTree(String),
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Firmware(#[from] android_firmware::Error),
    #[error("ext4 backend: {0}")]
    Ext4(#[from] fs_ext4::Error),
    #[error("filesystem IO: {0}")]
    Io(#[from] io::Error),
    #[error("tree manifest: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported or inconsistent image data: {0}")]
    Unsupported(String),
    #[error("EROFS backend: {0}")]
    Erofs(#[from] fs_erofs::Error),
}

/// Load a tree manifest. Blobs are trusted to match it: they are written once by an
/// edit and never changed in place.
pub fn read_tree(root: &Path) -> Result<PartitionTree, Error> {
    let tree: PartitionTree = serde_json::from_slice(&fs::read(root.join("manifest.json"))?)?;
    tree.validate()?;
    Ok(tree)
}

pub fn write_tree(root: &Path, tree: &PartitionTree) -> Result<(), Error> {
    tree.validate()?;
    let file = io::BufWriter::new(fs::File::create(root.join("manifest.json"))?);
    serde_json::to_writer(file, tree)?;
    Ok(())
}
