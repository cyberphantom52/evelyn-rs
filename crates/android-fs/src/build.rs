//! Construct EROFS and ext4 filesystem images.

use crate::PartitionTree;
use crate::{Error, Store};

mod erofs;
mod ext4;
use crate::Region;
use android_firmware::{Filesystem, Name};
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
    partition: &Name,
    mount_point: &str,
) -> Result<u64, Error> {
    let uuid = uuid(partition);
    match filesystem {
        Filesystem::Erofs => erofs::write(store, tree, region, uuid, EPOCH),
        Filesystem::Ext4 => {
            let label = if mount_point == "/" {
                "/"
            } else {
                partition.as_ref()
            };
            ext4::write(store, tree, region, label, uuid, EPOCH)
        }
    }
}

/// Build `tree` into a new image file at `output`, as [`image`] builds it in a region.
pub fn image_file(
    tree: &PartitionTree,
    store: &Store<'_>,
    output: &Path,
    filesystem: Filesystem,
    partition: &Name,
    mount_point: &str,
) -> Result<u64, Error> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(output)?;
    let region = Region::new(file.try_clone()?, 0);
    let size = image(tree, store, region, filesystem, partition, mount_point)?;
    file.set_len(size)?;
    Ok(size)
}

/// Stable UUID derived from the partition name, so rebuilds produce identical images.
fn uuid(partition: &Name) -> [u8; 16] {
    let digest = android_firmware::Sha256Digest::bytes(partition.as_ref().as_bytes());
    let mut uuid: [u8; 16] = std::array::from_fn(|at| digest.as_bytes()[at]);
    // Version 4, RFC 4122 variant.
    uuid[6] = 0x40 | (uuid[6] & 0x0f);
    uuid[8] = 0xa0 | (uuid[8] & 0x0f);
    uuid
}
