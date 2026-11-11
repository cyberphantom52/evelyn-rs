use crate::Error;
use crate::firmware::RelativePath;
use crate::image::{Image, Kind};
use crate::{Content, Entry, EntryKind, PartitionTree};
use std::collections::{BTreeMap, btree_map};

struct Scan<'a> {
    image: &'a Image,
    /// What the tree's contents call the image.
    index: usize,
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
                        EntryKind::File {
                            content: Content::Image {
                                image: self.index,
                                inode: id,
                            },
                            size: inode.size,
                        }
                    }
                }
            };
            self.tree.entries.insert(
                path.clone(),
                Entry {
                    kind,
                    metadata: inode.metadata,
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
/// entry with its Android metadata. Only metadata is read, no file's content.
/// Its files refer to the image as `index`, its place among those a
/// [`crate::Store`] reads.
pub fn scan(image: &Image, index: usize) -> Result<PartitionTree, Error> {
    let root_id = image.root()?;
    let root = image.inode(root_id)?;
    let mut extraction = Scan {
        image,
        index,
        tree: PartitionTree {
            filesystem: image.filesystem(),
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
