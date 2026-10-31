use crate::Error;
use crate::image::{Image, Kind};
use crate::{Content, Entry, EntryKind, PartitionTree, Provenance};
use android_firmware::{RelativePath, Sha256Digest, Sha256Writer, SourceRef};
use std::{
    collections::{BTreeMap, btree_map},
    io,
};

struct Scan<'a> {
    image: &'a Image,
    identity: &'a SourceRef,
    tree: PartitionTree,
    /// Inode -> (first path seen, links seen, link count recorded in the image).
    links: BTreeMap<u64, (RelativePath, u32, u32)>,
}

impl Scan<'_> {
    fn directory(&mut self, id: u64, parent: &str) -> Result<(), Error> {
        for (name, id) in self.image.children(id)? {
            let name = String::from_utf8(name)
                .map_err(|_| Error::Unsupported("non-UTF-8 filename".into()))?;
            let path = RelativePath::try_from(if parent.is_empty() {
                name
            } else {
                format!("{parent}/{name}")
            })?;
            let inode = self.image.inode(id)?;
            let kind = match (inode.kind, self.links.entry(id)) {
                (Kind::Directory, _) => EntryKind::Directory,
                (_, btree_map::Entry::Occupied(mut link)) => {
                    let (first, seen, _) = link.get_mut();
                    *seen += 1;
                    EntryKind::Hardlink {
                        target: first.clone(),
                    }
                }
                (kind, btree_map::Entry::Vacant(link)) => {
                    link.insert((path.clone(), 1, inode.links));
                    if kind == Kind::Symlink {
                        EntryKind::Symlink {
                            target: self.image.link(id)?,
                        }
                    } else {
                        let mut hash = Sha256Writer::new(io::sink());
                        let size = io::copy(&mut self.image.file(id, inode.size)?, &mut hash)?;
                        if size != inode.size {
                            return Err(Error::Unsupported(format!(
                                "short content read at {path}"
                            )));
                        }
                        EntryKind::File {
                            content: Content::Image {
                                source: self.identity.clone(),
                                inode: id,
                            },
                            sha256: hash.finish()?.1,
                            size,
                        }
                    }
                }
            };
            let provenance = Provenance::Image {
                source: None,
                partition: self.tree.partition.clone(),
                path: path.clone(),
                sha256: self.tree.source_sha256.clone(),
            };
            self.tree.entries.insert(
                path.clone(),
                Entry {
                    kind,
                    metadata: inode.metadata,
                    provenance: vec![provenance],
                },
            );
            if inode.kind == Kind::Directory {
                self.directory(id, path.as_ref())?;
            }
        }
        Ok(())
    }
}

/// Describe a source image as a tree whose file contents stay in the image: every
/// entry with its Android metadata, and each file's content hash.
pub fn scan(
    image: &Image,
    identity: &SourceRef,
    source_sha256: Sha256Digest,
) -> Result<PartitionTree, Error> {
    let root_id = image.root()?;
    let root = image.inode(root_id)?;
    let mut extraction = Scan {
        image,
        identity,
        tree: PartitionTree {
            schema: crate::SCHEMA,
            partition: identity.partition.clone(),
            filesystem: image.filesystem(),
            source_sha256,
            root: root.metadata,
            entries: BTreeMap::new(),
        },
        links: BTreeMap::new(),
    };
    extraction.directory(root_id, "")?;
    for (path, seen, expected) in extraction.links.values() {
        if seen != expected {
            return Err(Error::Unsupported(format!(
                "hardlink count mismatch at {path}: saw {seen}, expected {expected}"
            )));
        }
    }
    Ok(extraction.tree)
}
