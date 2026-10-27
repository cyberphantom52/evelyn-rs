//! Android filesystems: EROFS and ext4 images (raw or sparse) read in place, scanned into
//! partition trees, and written back out.

pub mod build;
pub mod device;
pub mod erofs;
pub mod image;
mod reader;
mod store;
mod writer;

use model::PartitionTree;
use std::{fs, io, path::Path};
use thiserror::Error;

pub use reader::scan;
pub use store::Store;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Model(#[from] model::Error),
    #[error("ext4 backend: {0}")]
    Ext4(#[from] fs_ext4::Error),
    #[error("filesystem IO: {0}")]
    Io(#[from] io::Error),
    #[error("tree manifest: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported or inconsistent image data: {0}")]
    Unsupported(String),
    #[error(transparent)]
    Tool(#[from] toolchain::Error),
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
