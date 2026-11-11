//! A partition as Evelyn works on it: every entry with its Android metadata, and where
//! each file's bytes are.

use crate::Error;
use android_firmware::{AndroidMetadata, Filesystem, Name, RelativePath};
use std::collections::BTreeMap;

/// Where a file's bytes are: unchanged in a source image, or in a tree's own store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    /// Inode `inode` of source image `image`: its index among those a
    /// [`crate::Store`] reads.
    Image { image: usize, inode: u64 },
    /// A file in the tree's `content/` directory, written by an edit.
    Blob(Name),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File { content: Content, size: u64 },
    Directory,
    Symlink { target: Vec<u8> },
    Hardlink { target: RelativePath },
}

/// An entry and its metadata: Android's, or what an editor of the tree keeps in its
/// place until it knows it.
#[derive(Clone, Debug)]
pub struct Entry<M = AndroidMetadata> {
    pub kind: EntryKind,
    pub metadata: M,
}

/// A regular file, at its canonical path: a hardlink's target.
pub struct File<'a> {
    pub path: &'a RelativePath,
    pub content: &'a Content,
    pub size: u64,
}

/// A partition's entries with their Android metadata. File bytes stay in the source
/// image until an edit writes a blob into the tree's `content/` directory.
#[derive(Clone, Debug)]
pub struct PartitionTree<M = AndroidMetadata> {
    pub filesystem: Filesystem,
    pub root: AndroidMetadata,
    pub entries: BTreeMap<RelativePath, Entry<M>>,
}

impl PartitionTree {
    pub fn blob_path(blob: &Name) -> RelativePath {
        RelativePath::try_from(format!("content/{blob}")).expect("blob names are safe components")
    }
}

impl<M> PartitionTree<M> {
    /// The regular file at `path`, following a hardlink.
    pub fn file(&self, path: &RelativePath) -> Result<File<'_>, Error> {
        let (mut path, mut entry) = self
            .entries
            .get_key_value(path)
            .ok_or_else(|| Error::Missing(path.clone()))?;
        if let EntryKind::Hardlink { target } = &entry.kind {
            (path, entry) = self
                .entries
                .get_key_value(target)
                .ok_or_else(|| Error::Missing(target.clone()))?;
        }
        match &entry.kind {
            EntryKind::File { content, size } => Ok(File {
                path,
                content,
                size: *size,
            }),
            _ => Err(Error::NotAFile(path.clone())),
        }
    }

    /// The same tree, each entry's metadata mapped by `metadata`.
    pub fn map<N, E>(
        self,
        mut metadata: impl FnMut(&RelativePath, &EntryKind, M) -> Result<N, E>,
    ) -> Result<PartitionTree<N>, E> {
        let entries = self
            .entries
            .into_iter()
            .map(|(path, entry)| {
                let metadata = metadata(&path, &entry.kind, entry.metadata)?;
                Ok((
                    path,
                    Entry {
                        kind: entry.kind,
                        metadata,
                    },
                ))
            })
            .collect::<Result<_, E>>()?;
        Ok(PartitionTree {
            filesystem: self.filesystem,
            root: self.root,
            entries,
        })
    }
}
