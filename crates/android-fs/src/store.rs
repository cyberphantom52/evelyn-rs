//! Where file contents are read from: source images and a tree's own blobs.

use crate::{Error, image::Source};
use android_firmware::SourceRef;
use model::{Content, PartitionTree};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

/// Opens file contents for one tree: `Image` contents from the open source images,
/// `Blob` contents from the tree's `content/` directory.
pub struct Store<'a> {
    pub images: &'a BTreeMap<SourceRef, Source>,
    pub root: &'a Path,
}

impl Store<'_> {
    pub fn open(&self, content: &Content, size: u64) -> Result<Box<dyn Read + '_>, Error> {
        match content {
            Content::Image { source, inode } => self
                .images
                .get(source)
                .ok_or_else(|| Error::Unsupported(format!("source {source} is not open")))?
                .file(*inode, size),
            Content::Blob(_) => Ok(Box::new(fs::File::open(self.blob_path(content))?)),
        }
    }

    /// The on-disk path of a `Blob` content.
    pub fn blob_path(&self, content: &Content) -> PathBuf {
        match content {
            Content::Blob(blob) => self.root.join(PartitionTree::blob_path(blob).as_ref()),
            Content::Image { .. } => unreachable!("image contents have no blob path"),
        }
    }
}
