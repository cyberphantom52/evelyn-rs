//! A boot or vendor_boot image's ramdisk, read as a partition: its cpio archive
//! decompressed once into memory, with an inode for each entry. Building one
//! back puts a new archive in the image, compressed the same way, and re-signs
//! it.

use crate::Error;
use crate::metadata::AndroidMetadata;
use bootimg::boot::Image;
use bootimg::compress::Compression;
use bootimg::cpio::{self, S_IFDIR, S_IFLNK, S_IFMT, S_IFREG};
use std::collections::BTreeMap;

/// The inode of the ramdisk's root; entry `n` of the archive is inode `n + 1`.
pub(crate) const ROOT: u64 = 0;

pub(crate) struct Ramdisk {
    /// The image the ramdisk is in, to build it back with a new one.
    pub image: Image,
    /// The image's component the ramdisk is.
    pub component: String,
    pub compression: Compression,
    entries: Vec<cpio::Entry>,
    /// Each directory's inode -> its children's names and inodes, sorted.
    children: BTreeMap<u64, Vec<(Vec<u8>, u64)>>,
    /// A hardlinked entry's inode -> the inode all its names share.
    links: BTreeMap<u64, u64>,
    /// Inode -> how many names it has.
    names: BTreeMap<u64, u32>,
    /// The archive's own entry for its root, which gives the root's metadata.
    root: Option<u64>,
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
        let (compression, archive) =
            bootimg::compress::detect_and_decompress(&image.components[&component])?;
        let entries = cpio::parse(&archive)?;

        // A later entry for a path replaces an earlier one, as when the kernel
        // unpacks the archive.
        let mut paths: BTreeMap<String, u64> = BTreeMap::new();
        let mut root = None;
        for (index, entry) in entries.iter().enumerate() {
            let path = entry.name.trim_start_matches("./").trim_matches('/');
            if !matches!(entry.mode & S_IFMT, S_IFDIR | S_IFREG | S_IFLNK) {
                return Err(Error::Unsupported(format!(
                    "special file {path} in the ramdisk, mode {:#o}",
                    entry.mode
                )));
            }
            match path {
                "" | "." => root = Some(index as u64 + 1),
                path => {
                    paths.insert(path.to_owned(), index as u64 + 1);
                }
            }
        }
        let mut links = BTreeMap::new();
        let mut names = BTreeMap::new();
        let mut first: BTreeMap<u32, u64> = BTreeMap::new();
        for &inode in paths.values() {
            let entry = &entries[inode as usize - 1];
            let canonical = if entry.mode & S_IFMT == S_IFREG && entry.nlink > 1 {
                *first.entry(entry.ino).or_insert(inode)
            } else {
                inode
            };
            links.insert(inode, canonical);
            *names.entry(canonical).or_insert(0) += 1;
        }
        let mut children: BTreeMap<u64, Vec<(Vec<u8>, u64)>> = BTreeMap::new();
        for (path, &inode) in &paths {
            let (parent, name) = match path.rsplit_once('/') {
                Some((parent, name)) => (
                    *paths.get(parent).ok_or_else(|| {
                        Error::Unsupported(format!("{path} in the ramdisk has no parent entry"))
                    })?,
                    name,
                ),
                None => (ROOT, path.as_str()),
            };
            children
                .entry(parent)
                .or_default()
                .push((name.as_bytes().to_vec(), links[&inode]));
        }
        for list in children.values_mut() {
            list.sort();
        }
        Ok(Self {
            image,
            component,
            compression,
            entries,
            children,
            links,
            names,
            root,
        })
    }

    pub(crate) fn children(&self, inode: u64) -> Vec<(Vec<u8>, u64)> {
        self.children.get(&inode).cloned().unwrap_or_default()
    }

    /// The type bits of an inode's mode, its size, its number of names and its
    /// metadata.
    pub(crate) fn inode(&self, inode: u64) -> (u16, u64, u32, AndroidMetadata) {
        let Some(entry) = self.entry(inode) else {
            let metadata = AndroidMetadata {
                uid: 0,
                gid: 0,
                mode: 0o755,
                xattrs: BTreeMap::new(),
            };
            return (crate::mode::DIRECTORY, 0, 1, metadata);
        };
        let metadata = AndroidMetadata {
            uid: entry.uid,
            gid: entry.gid,
            mode: (entry.mode & 0o7777) as u16,
            xattrs: BTreeMap::new(),
        };
        let names = self.names.get(&inode).copied().unwrap_or(1);
        let kind = (entry.mode & S_IFMT) as u16;
        (kind, self.data(inode).len() as u64, names, metadata)
    }

    /// A file's contents or a symlink's target. Of a hardlinked file's names,
    /// the archive may carry the data in any one.
    pub(crate) fn data(&self, inode: u64) -> &[u8] {
        let Some(entry) = self.entry(inode) else {
            return &[];
        };
        if entry.mode & S_IFMT == S_IFREG && entry.nlink > 1 && entry.data.is_empty() {
            let linked = self
                .links
                .iter()
                .filter(|(_, canonical)| **canonical == inode)
                .filter_map(|(name, _)| self.entry(*name))
                .find(|entry| !entry.data.is_empty());
            if let Some(linked) = linked {
                return &linked.data;
            }
        }
        &entry.data
    }

    fn entry(&self, inode: u64) -> Option<&cpio::Entry> {
        let index = match inode {
            ROOT => self.root?,
            inode => inode,
        };
        self.entries.get(index as usize - 1)
    }
}
