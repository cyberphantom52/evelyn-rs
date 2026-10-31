//! Where file contents are read from: source images and a tree's own blobs.

use crate::{Content, Error, File, Image, PartitionTree};
use android_firmware::{Name, SourceRef};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

/// Opens file contents: `Image` contents from the open source images, and `Blob`
/// contents from a tree's `content/` directory, when it has one.
pub struct Store<'a> {
    images: &'a BTreeMap<SourceRef, Image>,
    root: Option<&'a Path>,
}

impl<'a> Store<'a> {
    /// The contents of the tree at `root`.
    pub fn new(images: &'a BTreeMap<SourceRef, Image>, root: &'a Path) -> Self {
        Self {
            images,
            root: Some(root),
        }
    }

    /// The contents of a scanned source tree, which are all in its image.
    pub fn sources(images: &'a BTreeMap<SourceRef, Image>) -> Self {
        Self { images, root: None }
    }

    pub fn open(&self, content: &Content, size: u64) -> Result<Box<dyn Read + 'a>, Error> {
        match content {
            Content::Image { source, inode } => self
                .images
                .get(source)
                .ok_or_else(|| Error::Unsupported(format!("source {source} is not open")))?
                .file(*inode, size),
            Content::Blob(blob) => Ok(Box::new(fs::File::open(self.blob_path(blob)?)?)),
        }
    }

    /// The whole content of `file`.
    pub fn read(&self, file: &File<'_>) -> Result<Vec<u8>, Error> {
        let mut bytes = Vec::with_capacity(file.size as usize);
        self.open(file.content, file.size)?
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    fn blob_path(&self, blob: &Name) -> Result<PathBuf, Error> {
        let root = self
            .root
            .ok_or_else(|| Error::Unsupported(format!("blob {blob} outside a tree")))?;
        Ok(root.join(PartitionTree::blob_path(blob).as_ref()))
    }
}
