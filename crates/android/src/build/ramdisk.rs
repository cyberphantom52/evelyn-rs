//! A ramdisk tree built back into the boot image it came from.

use crate::path::RelativePath;
use crate::ramdisk::Ramdisk;
use crate::{EntryKind, Error, IoAt, PartitionTree, Store};
use bootimg::cpio::{self, S_IFDIR, S_IFLNK, S_IFREG};
use std::{collections::BTreeMap, fs, io::Read, path::Path};

/// Write `source`'s boot image to `output` with `tree` as its ramdisk, compressed
/// as the source's was, and its GKI signature and AVB footer re-signed with the
/// AOSP test keys of the original keys' sizes. Returns the image's size.
pub(super) fn write(
    store: &Store<'_>,
    tree: &PartitionTree,
    source: &Ramdisk,
    output: &Path,
) -> Result<u64, Error> {
    // Each file's inode number and names: its own path, and the hardlinks to it.
    // The kernel takes a hardlinked file's data from the first of its names in
    // the archive.
    let mut files: BTreeMap<&RelativePath, (u32, u32, Option<&RelativePath>)> = BTreeMap::new();
    for (index, (path, entry)) in tree.entries.iter().enumerate() {
        let canonical = match &entry.kind {
            EntryKind::File { .. } => path,
            EntryKind::Hardlink { target } => target,
            _ => continue,
        };
        let file = files
            .entry(canonical)
            .or_insert((index as u32 + 1, 0, None));
        file.1 += 1;
        file.2.get_or_insert(path);
    }

    let mut entries = Vec::with_capacity(tree.entries.len());
    for (index, (path, entry)) in tree.entries.iter().enumerate() {
        let (kind, metadata, data, ino, nlink) = match &entry.kind {
            EntryKind::Directory => (S_IFDIR, &entry.metadata, Vec::new(), index as u32 + 1, 1),
            EntryKind::Symlink { target } => (
                S_IFLNK,
                &entry.metadata,
                target.clone(),
                index as u32 + 1,
                1,
            ),
            EntryKind::File { .. } | EntryKind::Hardlink { .. } => {
                let file = tree.file(path)?;
                let (ino, nlink, first) = files[file.path];
                let data = if first == Some(path) {
                    let mut data = Vec::with_capacity(file.size as usize);
                    store
                        .open(file.content, file.size)?
                        .read_to_end(&mut data)
                        .entry(path)?;
                    data
                } else {
                    Vec::new()
                };
                let metadata = &tree.entries[file.path].metadata;
                (S_IFREG, metadata, data, ino, nlink)
            }
        };
        entries.push(cpio::Entry {
            name: path.to_string(),
            ino,
            mode: kind | u32::from(metadata.mode),
            uid: metadata.uid,
            gid: metadata.gid,
            nlink,
            data,
            ..Default::default()
        });
    }

    let ramdisk = source.compression.compress(&cpio::write(&entries))?;
    let mut image = source.image.clone();
    image.components.insert(source.component.clone(), ramdisk);
    let mut file = fs::File::create(output).at(output)?;
    image.write(&mut file, None)?;
    file.metadata().at(output).map(|metadata| metadata.len())
}
