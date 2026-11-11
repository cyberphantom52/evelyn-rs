//! A partition as Evelyn works on it: every entry with its Android metadata, and where
//! each file's bytes are.

use crate::Error;
use android_firmware::{AndroidMetadata, Filesystem, Name, RelativePath, Sha256Digest, SourceRef};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Where a file's bytes are: unchanged in a source image, or in a tree's own store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Content {
    /// Inode `inode` of source image `source`.
    Image { source: SourceRef, inode: u64 },
    /// A file in the tree's `content/` directory, written by an edit.
    Blob(Name),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum EntryKind {
    File {
        content: Content,
        sha256: Sha256Digest,
        size: u64,
    },
    Directory,
    Symlink {
        target: Vec<u8>,
    },
    Hardlink {
        target: RelativePath,
    },
}

/// An entry and its metadata: Android's, or what an editor of the tree keeps in its
/// place until it knows it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry<M = AndroidMetadata> {
    pub kind: EntryKind,
    pub metadata: M,
}

/// The manifest schema this crate reads and writes.
pub const SCHEMA: u32 = 3;

/// A regular file, at its canonical path: a hardlink's target.
pub struct File<'a> {
    pub path: &'a RelativePath,
    pub content: &'a Content,
    pub sha256: &'a Sha256Digest,
    pub size: u64,
}

/// A partition's entries with their Android metadata. File bytes stay in the source
/// image until an edit writes a blob into the tree's `content/` directory.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartitionTree<M = AndroidMetadata> {
    pub schema: u32,
    pub partition: Name,
    pub filesystem: Filesystem,
    pub source_sha256: Sha256Digest,
    pub root: AndroidMetadata,
    pub entries: BTreeMap<RelativePath, Entry<M>>,
}

impl PartitionTree {
    pub fn validate(&self) -> Result<(), Error> {
        let invalid = Error::InvalidTree;
        if self.schema != SCHEMA {
            return Err(invalid(format!("unsupported schema {}", self.schema)));
        }
        let mut blobs = BTreeSet::new();
        for (path, entry) in &self.entries {
            if let Some((parent, _)) = path.as_ref().rsplit_once('/') {
                let parent = RelativePath::try_from(parent.to_owned())?;
                if !self
                    .entries
                    .get(&parent)
                    .is_some_and(|entry| matches!(entry.kind, EntryKind::Directory))
                {
                    return Err(invalid(format!("missing directory parent for {path}")));
                }
            }
            match &entry.kind {
                EntryKind::File {
                    content: Content::Blob(blob),
                    ..
                } if !blobs.insert(blob) => {
                    return Err(invalid(format!(
                        "duplicate writable blob {blob}; use an explicit hardlink"
                    )));
                }
                EntryKind::Symlink { target } if target.is_empty() || target.contains(&0) => {
                    return Err(invalid(format!("invalid symlink target at {path}")));
                }
                EntryKind::Hardlink { target } => {
                    let canonical = self
                        .entries
                        .get(target)
                        .ok_or_else(|| invalid(format!("missing hardlink target {target}")))?;
                    if path == target
                        || !matches!(
                            canonical.kind,
                            EntryKind::File { .. } | EntryKind::Symlink { .. }
                        )
                        || canonical.metadata != entry.metadata
                    {
                        return Err(invalid(format!(
                            "invalid or inconsistent hardlink {path} -> {target}"
                        )));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn blob_path(blob: &Name) -> RelativePath {
        RelativePath::try_from(format!("content/{blob}"))
            .expect("validated blob names are safe components")
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
            EntryKind::File {
                content,
                sha256,
                size,
            } => Ok(File {
                path,
                content,
                sha256,
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
            schema: self.schema,
            partition: self.partition,
            filesystem: self.filesystem,
            source_sha256: self.source_sha256,
            root: self.root,
            entries,
        })
    }
}
