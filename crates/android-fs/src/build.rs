//! Construct EROFS and ext4 filesystem images.

use crate::PartitionTree;
use crate::{Error, Store};

mod erofs;
mod ext4;
use android_firmware::{Filesystem, Name};
use std::{fs, path::Path};

/// Fixed timestamp for reproducible images (2009-01-01, as AOSP builds use).
const EPOCH: u64 = 1_230_768_000;

/// Build `tree`, reading file contents from `store`, into a new image at `output`.
/// Returns the image size.
pub fn image(
    tree: &PartitionTree,
    store: &Store<'_>,
    output: &Path,
    filesystem: Filesystem,
    partition: &Name,
    mount_point: &str,
) -> Result<u64, Error> {
    let uuid = uuid(partition);
    match filesystem {
        Filesystem::Erofs => erofs::write(store, tree, output, uuid, EPOCH)?,
        Filesystem::Ext4 => {
            let label = if mount_point == "/" {
                "/"
            } else {
                partition.as_ref()
            };
            ext4::write(store, tree, output, label, uuid, EPOCH)?;
        }
    }
    Ok(fs::metadata(output)?.len())
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
