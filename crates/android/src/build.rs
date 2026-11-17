//! Construct EROFS and ext4 filesystem images, and boot images with a new ramdisk.

use crate::{Error, Image, IoAt, PartitionTree, Store};

mod erofs;
mod ext4;
mod ramdisk;
use crate::Filesystem;
use crate::Region;
use std::{fs, path::Path};

/// Fixed timestamp for reproducible images (2009-01-01, as AOSP builds use).
const EPOCH: u64 = 1_230_768_000;

/// Build `tree` in `region`, reading file contents from `store`. Returns the image's
/// size.
pub fn image(
    tree: &PartitionTree,
    store: &Store<'_>,
    region: Region,
    filesystem: Filesystem,
    partition: &str,
    mount_point: &str,
) -> Result<u64, Error> {
    let uuid = uuid(partition);
    match filesystem {
        Filesystem::Erofs => erofs::write(store, tree, region, uuid, EPOCH),
        Filesystem::Ext4 => {
            let label = if mount_point == "/" { "/" } else { partition };
            ext4::write(store, tree, region, label, uuid, EPOCH)
        }
        Filesystem::Ramdisk => Err(Error::Unsupported(
            "a ramdisk is built into its boot image, not on its own".into(),
        )),
    }
    .map_err(|source| Error::Build {
        partition: partition.to_owned(),
        source: Box::new(source),
    })
}

/// Build `tree` into a new image file at `output`, as [`image`] builds it in a region.
pub fn image_file(
    tree: &PartitionTree,
    store: &Store<'_>,
    output: &Path,
    filesystem: Filesystem,
    partition: &str,
    mount_point: &str,
) -> Result<u64, Error> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(output)
        .at(output)?;
    let region = Region::new(file.try_clone().at(output)?, 0);
    let size = image(tree, store, region, filesystem, partition, mount_point)?;
    file.set_len(size).at(output)?;
    Ok(size)
}

/// Build the boot image `source` is the ramdisk of into a new file at `output`,
/// with `tree` as its ramdisk. Returns the image's size.
pub fn boot_image(
    tree: &PartitionTree,
    store: &Store<'_>,
    source: &Image,
    output: &Path,
    partition: &str,
) -> Result<u64, Error> {
    let ramdisk = source
        .ramdisk()
        .ok_or_else(|| Error::Unsupported(format!("{partition} is not built from a boot image")))?;
    ramdisk::write(store, tree, ramdisk, output).map_err(|source| Error::Build {
        partition: partition.to_owned(),
        source: Box::new(source),
    })
}

/// Stable UUID derived from the partition name, so rebuilds produce identical images.
fn uuid(partition: &str) -> [u8; 16] {
    let digest = crate::hash::Sha256Digest::bytes(partition.as_bytes());
    let mut uuid: [u8; 16] = std::array::from_fn(|at| digest.as_bytes()[at]);
    // Version 4, RFC 4122 variant.
    uuid[6] = 0x40 | (uuid[6] & 0x0f);
    uuid[8] = 0xa0 | (uuid[8] & 0x0f);
    uuid
}
