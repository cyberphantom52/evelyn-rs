//! A ramdisk tree built back into the boot image it came from.

use crate::ramdisk::Ramdisk;
use crate::{EntryKind, Error, IoAt, PartitionTree, Store};
use bootimg::avb::Signer;
use bootimg::ramdisk::{self, Kind, Node};
use std::{fs, io::Read, path::Path};

/// Write `image` to `output` with `tree` as its ramdisk, compressed as
/// `source`'s was. Its GKI signature and AVB footer are re-signed with the
/// AOSP test keys of the original keys' sizes. Returns the image's size.
pub(super) fn write(
    store: &Store<'_>,
    tree: &PartitionTree,
    source: &Ramdisk,
    image: &bootimg::Image,
    output: &Path,
) -> Result<u64, Error> {
    let mut entries = std::collections::BTreeMap::new();
    for (path, entry) in &tree.entries {
        let kind = match &entry.kind {
            EntryKind::Directory => Kind::Directory,
            EntryKind::Symlink { target } => Kind::Symlink(target.clone()),
            EntryKind::Hardlink { target } => Kind::Hardlink(target.to_string()),
            EntryKind::File { content, size } => {
                let mut data = Vec::with_capacity(*size as usize);
                store
                    .open(content, *size)?
                    .read_to_end(&mut data)
                    .entry(path)?;
                Kind::File(data)
            }
        };
        let metadata = &entry.metadata;
        entries.insert(
            path.to_string(),
            Node {
                kind,
                mode: u32::from(metadata.mode),
                uid: metadata.uid,
                gid: metadata.gid,
                mtime: 0,
            },
        );
    }
    let ramdisk = ramdisk::Ramdisk {
        compression: source.tree.compression,
        entries,
    }
    .build()?;

    let mut image = image.clone();
    *image.content.ramdisks_mut().remove(0) = ramdisk;
    let mut file = fs::File::create(output).at(output)?;
    image.write(&mut file, &Signer::TestKeys)?;
    file.metadata().at(output).map(|metadata| metadata.len())
}
