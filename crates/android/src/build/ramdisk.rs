//! A ramdisk tree built back into the boot image it came from.

use crate::ramdisk::Ramdisk;
use crate::{EntryKind, Error, IoAt, PartitionTree, Store};
use bootimg::avb::Signer;
use bootimg::compress::Compression;
use bootimg::ramdisk::{self, Kind, Node};
use std::{fs, io::Read, path::Path};

/// Write `image` to `output` with `tree` as its ramdisk, in the format
/// `source`'s was compressed in. Its GKI signature and AVB footer are re-signed with the
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
    let compression = source.tree.compression;
    let ramdisk = ramdisk::Ramdisk {
        compression,
        entries,
    }
    .build(level(compression))?;

    let mut image = image.clone();
    *image.content.ramdisks_mut().remove(0) = ramdisk;
    let mut file = fs::File::create(output).at(output)?;
    image.write(&mut file, &Signer::TestKeys)?;
    file.metadata().at(output).map(|metadata| metadata.len())
}

/// The level a ramdisk is compressed at, which every build pays for: LZ4 HC 9
/// takes a tenth of the time of 12 for a ramdisk under 1% larger. Gzip, xz
/// and lzma stay at their default levels.
fn level(compression: Compression) -> u32 {
    match compression {
        Compression::None => 0,
        Compression::Lz4Legacy | Compression::Lz4Frame => 9,
        Compression::Gzip | Compression::Xz { .. } | Compression::Lzma => 6,
    }
}
