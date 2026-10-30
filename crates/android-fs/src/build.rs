//! Construct filesystem images: EROFS with the pinned mkfs.erofs, ext4 in-process.

use crate::{Error, Store};
use model::{Filesystem, Name, PartitionTree};
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
                    .args(["-T", &EPOCH.to_string(), "-U", &hyphenated(uuid)])
                    .arg(output),
                |stdin| crate::writer::write_tar(store, tree, BufWriter::new(stdin), EPOCH),
            )?;
        }
        Filesystem::Ext4 => {
            let label = if mount_point == "/" {
                "/"
            } else {
                partition.as_ref()
            };
            crate::writer::write_ext4(store, tree, output, label, uuid, EPOCH)?;
        }
    }
    Ok(fs::metadata(output)?.len())
}

/// Stable UUID derived from the partition name, so rebuilds produce identical images.
fn uuid(partition: &Name) -> [u8; 16] {
    let hex = model::Sha256Digest::bytes(partition.as_ref().as_bytes()).to_string();
    let mut uuid: [u8; 16] = std::array::from_fn(|at| {
        u8::from_str_radix(&hex[2 * at..2 * at + 2], 16).expect("a SHA-256 digest is hex")
    });
    // Version 4, RFC 4122 variant.
    uuid[6] = 0x40 | (uuid[6] & 0x0f);
    uuid[8] = 0xa0 | (uuid[8] & 0x0f);
    uuid
}

fn hyphenated(uuid: [u8; 16]) -> String {
    let hex: String = uuid.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}
