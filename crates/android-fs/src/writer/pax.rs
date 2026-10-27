use crate::{Error, Store};
use base64::{Engine, engine::general_purpose::STANDARD};
use model::{AndroidMetadata, EntryKind, PartitionTree};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use std::{
    ffi::OsStr,
    io::{self, Write},
    os::unix::ffi::OsStrExt,
    path::Path,
};

fn header(metadata: &AndroidMetadata, size: u64, epoch: u64, kind: tar::EntryType) -> tar::Header {
    let mut header = tar::Header::new_gnu();
    header.set_uid(u64::from(metadata.uid));
    header.set_gid(u64::from(metadata.gid));
    header.set_mode(u32::from(metadata.mode));
    header.set_mtime(epoch);
    header.set_size(size);
    header.set_entry_type(kind);
    header
}

/// Queue the entry's xattrs as libarchive-style PAX records, which mkfs.erofs understands.
fn xattrs<W: Write>(
    archive: &mut tar::Builder<W>,
    metadata: &AndroidMetadata,
) -> Result<(), Error> {
    if metadata.xattrs.is_empty() {
        return Ok(());
    }
    let encoded: Vec<_> = metadata
        .xattrs
        .iter()
        .map(|(name, value)| {
            let name = utf8_percent_encode(name, NON_ALPHANUMERIC);
            (format!("LIBARCHIVE.xattr.{name}"), STANDARD.encode(value))
        })
        .collect();
    archive.append_pax_extensions(
        encoded
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_bytes())),
    )?;
    Ok(())
}

/// Stream the tree as a PAX archive for `mkfs.erofs --tar`.
pub fn write_tar<W: Write>(
    store: &Store<'_>,
    tree: &PartitionTree,
    destination: W,
    epoch: u64,
) -> Result<(), Error> {
    let mut archive = tar::Builder::new(destination);
    xattrs(&mut archive, &tree.root)?;
    archive.append_data(
        &mut header(&tree.root, 0, epoch, tar::EntryType::Directory),
        ".",
        io::empty(),
    )?;
    for (path, entry) in &tree.entries {
        if matches!(entry.kind, EntryKind::Hardlink { .. }) {
            continue;
        }
        xattrs(&mut archive, &entry.metadata)?;
        match &entry.kind {
            EntryKind::Directory => archive.append_data(
                &mut header(&entry.metadata, 0, epoch, tar::EntryType::Directory),
                path.as_ref(),
                io::empty(),
            )?,
            EntryKind::File { content, size, .. } => {
                let file = store.open(content, *size)?;
                archive.append_data(
                    &mut header(&entry.metadata, *size, epoch, tar::EntryType::Regular),
                    path.as_ref(),
                    file,
                )?;
            }
            EntryKind::Symlink { target } => {
                archive.append_link(
                    &mut header(&entry.metadata, 0, epoch, tar::EntryType::Symlink),
                    path.as_ref(),
                    Path::new(OsStr::from_bytes(target)),
                )?;
            }
            EntryKind::Hardlink { .. } => unreachable!(),
        }
    }
    // Canonical entries must exist before tar hardlink references, regardless of path order.
    for (path, entry) in &tree.entries {
        if let EntryKind::Hardlink { target } = &entry.kind {
            archive.append_link(
                &mut header(&entry.metadata, 0, epoch, tar::EntryType::Link),
                path.as_ref(),
                target.as_ref(),
            )?;
        }
    }
    archive.into_inner()?.flush()?;
    Ok(())
}
