//! The vendor fstab says which dynamic partitions the device mounts and which
//! filesystems each may use. A build must supply every `logical` entry that is not
//! `nofail`, in a filesystem the entry accepts.
//!
//! Only the composed `vendor/etc/fstab.*` is read. The first-stage copy inside
//! `vendor_boot` is patched outside Evelyn and normally matches it.

use crate::{Error, build::Targets};
use android_firmware::Filesystem;
use model::EntryKind;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
};

#[derive(Default)]
struct Mount {
    filesystems: BTreeSet<String>,
    required: bool,
}

pub(crate) fn check(targets: &Targets, images: &patch::Images) -> Result<(), Error> {
    let Some((root, tree)) = targets
        .iter()
        .find(|(name, _)| name.as_ref() == "vendor")
        .map(|(_, target)| target)
    else {
        tracing::warn!("no vendor partition; its fstab cannot be checked");
        return Ok(());
    };
    let store = android_fs::Store { images, root };
    let mut checked = false;
    for (path, entry) in &tree.entries {
        let Some(name) = path.as_ref().strip_prefix("etc/fstab.") else {
            continue;
        };
        let entry = match &entry.kind {
            EntryKind::Hardlink { target } => &tree.entries[target],
            _ => entry,
        };
        let EntryKind::File { content, size, .. } = &entry.kind else {
            continue;
        };
        let mut text = String::new();
        if store
            .open(content, *size)?
            .read_to_string(&mut text)
            .is_err()
        {
            continue;
        }
        checked = true;
        check_one(&format!("fstab.{name}"), &text, targets)?;
    }
    if !checked {
        tracing::warn!("vendor has no etc/fstab.*; the partitions it mounts cannot be checked");
    }
    Ok(())
}

fn check_one(fstab: &str, text: &str, targets: &Targets) -> Result<(), Error> {
    let mut mounts: BTreeMap<&str, Mount> = BTreeMap::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [source, _, filesystem, _, flags, ..] = fields.as_slice() else {
            continue;
        };
        let flags: Vec<&str> = flags.split(',').collect();
        if !flags.contains(&"logical") {
            continue;
        }
        let mount = mounts.entry(source).or_default();
        mount.filesystems.insert((*filesystem).to_owned());
        mount.required |= !flags.contains(&"nofail");
    }
    for (partition, mount) in mounts {
        match targets.iter().find(|(name, _)| name.as_ref() == partition) {
            Some((_, (_, tree))) => {
                let filesystem = match tree.filesystem {
                    Filesystem::Erofs => "erofs",
                    Filesystem::Ext4 => "ext4",
                };
                if !mount.filesystems.contains(filesystem) {
                    return Err(Error::FstabFilesystem {
                        fstab: fstab.into(),
                        partition: partition.into(),
                        filesystem: filesystem.into(),
                        accepted: mount.filesystems.into_iter().collect(),
                    });
                }
            }
            None if mount.required => {
                return Err(Error::FstabPartition {
                    fstab: fstab.into(),
                    partition: partition.into(),
                });
            }
            None => {}
        }
    }
    Ok(())
}
