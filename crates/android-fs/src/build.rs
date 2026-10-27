//! Construct filesystem and super images with the pinned host tools.

use crate::{Error, Store};
use model::{EntryKind, Filesystem, Name, PartitionTree};
use std::{fs, io::BufWriter, path::Path};
use toolchain::Tools;

/// Fixed timestamp for reproducible images (2009-01-01, as AOSP builds use).
const EPOCH: u64 = 1_230_768_000;

/// Build `tree`, reading file contents from `store`, into a new image at `output`.
/// Returns the image size.
pub fn image(
    tools: &Tools,
    tree: &PartitionTree,
    store: &Store<'_>,
    output: &Path,
    filesystem: Filesystem,
    partition: &Name,
    mount_point: &str,
) -> Result<u64, Error> {
    let uuid = uuid(partition);
    match filesystem {
        Filesystem::Erofs => {
            // The tree streams into mkfs.erofs as a PAX tarball on its stdin.
            toolchain::run_with_input(
                tools
                    .command("mkfs.erofs")?
                    .args(["--quiet", "-zlz4hc,12", "-C", "32768", "--tar=f"])
                    .args(["-T", &EPOCH.to_string(), "-U", &uuid])
                    .arg(output),
                |stdin| crate::writer::write_tar(store, tree, BufWriter::new(stdin), EPOCH),
            )?;
        }
        Filesystem::Ext4 => {
            let (blocks, inodes) = ext4_size(tree);
            let label = if mount_point == "/" {
                "/"
            } else {
                partition.as_ref()
            };
            toolchain::run(
                tools
                    .command("mke2fs.android")?
                    .args(["-O", "^has_journal", "-L", label, "-I", "256"])
                    .args(["-N", &inodes.to_string(), "-M", mount_point])
                    .args(["-m", "0", "-t", "ext4", "-b", "4096", "-U", &uuid])
                    .args(["-E", &format!("hash_seed={uuid}")])
                    .env("E2FSPROGS_FAKE_TIME", EPOCH.to_string())
                    .arg(output)
                    .arg(blocks.to_string()),
            )?;
            crate::writer::populate_ext4(store, tree, output, EPOCH)?;
        }
    }
    Ok(fs::metadata(output)?.len())
}

/// Content blocks plus 10% and 4 MiB of headroom, like the AOSP build defaults.
fn ext4_size(tree: &PartitionTree) -> (u64, u64) {
    let blocks: u64 = tree
        .entries
        .values()
        .map(|entry| match &entry.kind {
            EntryKind::File { size, .. } => size.div_ceil(4096),
            EntryKind::Hardlink { .. } => 0,
            _ => 1,
        })
        .sum();
    (
        blocks + blocks / 10 + 1024,
        tree.entries.len() as u64 + 1000,
    )
}

/// Stable UUID derived from the partition name, so rebuilds produce identical images.
fn uuid(partition: &Name) -> String {
    let hex = model::Sha256Digest::bytes(partition.as_ref().as_bytes()).to_string();
    format!(
        "{}-{}-4{}-a{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[13..16],
        &hex[17..20],
        &hex[20..32]
    )
}
