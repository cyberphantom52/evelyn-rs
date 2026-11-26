//! A boot or vendor_boot image's ramdisk, read as a partition: bootimg's tree
//! of it, with an inode for each path. Building one back puts a new archive in
//! the image, compressed the same way, and re-signs it.

use crate::Error;
use crate::metadata::AndroidMetadata;
use bootimg::boot::Image;
use bootimg::ramdisk::{self, Kind, Node};
use std::collections::BTreeMap;

/// The inode of the ramdisk's root; the other paths number from 1 in order.
pub(crate) const ROOT: u64 = 0;

pub(crate) struct Ramdisk {
    /// The image the ramdisk is in, to build it back with a new one.
    pub image: Image,
    /// The image's component the ramdisk is.
    pub component: String,
    pub tree: ramdisk::Ramdisk,
    /// Inode -> path: the root, then every other path in order.
    paths: Vec<String>,
    /// Each directory's inode -> its children's names and inodes, sorted.
    children: BTreeMap<u64, Vec<(Vec<u8>, u64)>>,
    /// A hardlinked file's inode -> how many names it has.
    names: BTreeMap<u64, u32>,
}

impl Ramdisk {
    /// Read the ramdisk of the boot image `data`, which must have exactly one.
    pub(crate) fn parse(data: &[u8]) -> Result<Self, Error> {
        let image = Image::parse(data)?;
        let component = match image.ramdisks().as_slice() {
            [one] => one.to_string(),
            [] => return Err(Error::Unsupported("a boot image without a ramdisk".into())),
            more => {
                return Err(Error::Unsupported(format!(
                    "a boot image with {} ramdisks",
                    more.len()
                )));
            }
        };
        let tree = ramdisk::Ramdisk::parse(&image.components[&component])?;
        // The root sorts first, so the list stays sorted for lookups.
        let paths: Vec<String> = std::iter::once(String::new())
            .chain(tree.entries.keys().filter(|path| !path.is_empty()).cloned())
            .collect();
        let inode = |path: &str| {
            paths
                .binary_search_by(|p| p.as_str().cmp(path))
                .ok()
                .map(|index| index as u64)
        };

        let mut children: BTreeMap<u64, Vec<(Vec<u8>, u64)>> = BTreeMap::new();
        let mut names = BTreeMap::new();
        for (path, node) in tree.entries.iter().filter(|(path, _)| !path.is_empty()) {
            let target = match &node.kind {
                Kind::Special { kind, .. } => {
                    return Err(Error::Unsupported(format!(
                        "special file {path} in the ramdisk, mode {:#o}",
                        kind | node.mode
                    )));
                }
                Kind::Hardlink(file) => {
                    let file = inode(file).unwrap();
                    *names.entry(file).or_insert(1) += 1;
                    file
                }
                _ => inode(path).unwrap(),
            };
            let (parent, name) = match path.rsplit_once('/') {
                Some((parent, name)) => (
                    inode(parent).ok_or_else(|| {
                        Error::Unsupported(format!("{path} in the ramdisk has no parent entry"))
                    })?,
                    name,
                ),
                None => (ROOT, path.as_str()),
            };
            children
                .entry(parent)
                .or_default()
                .push((name.as_bytes().to_vec(), target));
        }
        for list in children.values_mut() {
            list.sort();
        }
        Ok(Self {
            image,
            component,
            tree,
            paths,
            children,
            names,
        })
    }

    pub(crate) fn children(&self, inode: u64) -> Vec<(Vec<u8>, u64)> {
        self.children.get(&inode).cloned().unwrap_or_default()
    }

    /// The type bits of an inode's mode, its size, its number of names and its
    /// metadata.
    pub(crate) fn inode(&self, inode: u64) -> (u16, u64, u32, AndroidMetadata) {
        let Some(node) = self.node(inode) else {
            let metadata = AndroidMetadata {
                uid: 0,
                gid: 0,
                mode: 0o755,
                xattrs: BTreeMap::new(),
            };
            return (crate::mode::DIRECTORY, 0, 1, metadata);
        };
        let metadata = AndroidMetadata {
            uid: node.uid,
            gid: node.gid,
            mode: node.mode as u16,
            xattrs: BTreeMap::new(),
        };
        let names = self.names.get(&inode).copied().unwrap_or(1);
        let kind = node.kind.type_bits() as u16;
        (kind, self.data(inode).len() as u64, names, metadata)
    }

    /// A file's contents or a symlink's target.
    pub(crate) fn data(&self, inode: u64) -> &[u8] {
        match self.node(inode).map(|node| &node.kind) {
            Some(Kind::File(data) | Kind::Symlink(data)) => data,
            _ => &[],
        }
    }

    /// An inode's node; the root has one only if the archive has an entry for it.
    fn node(&self, inode: u64) -> Option<&Node> {
        self.tree.entries.get(&self.paths[inode as usize])
    }
}
