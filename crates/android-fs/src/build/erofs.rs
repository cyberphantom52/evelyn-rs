use crate::{EntryKind, PartitionTree};
use crate::{Error, Region, Store};
use android_firmware::AndroidMetadata;
use fs_erofs::mkfs::{self, BuildOptions, Compression, Content, Node, NodeMeta, XattrSpec};
use std::{
    collections::BTreeMap,
    io::{self, BufWriter, Write},
};

fn xattrs(metadata: &AndroidMetadata) -> Result<Vec<XattrSpec>, Error> {
    metadata
        .xattrs
        .iter()
        .map(|(name, value)| Ok(XattrSpec::from_full_name(name, value.clone())?))
        .collect()
}

fn meta(metadata: &AndroidMetadata) -> NodeMeta {
    NodeMeta {
        uid: metadata.uid,
        gid: metadata.gid,
        ..NodeMeta::default()
    }
}

/// Add `node` at `path` under the directory entries of the root.
fn insert<'a>(
    root: &mut BTreeMap<String, Node<'a>>,
    path: &str,
    node: Node<'a>,
) -> Result<(), Error> {
    let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
    let mut entries = root;
    for component in parent.split('/').filter(|component| !component.is_empty()) {
        entries = match entries.get_mut(component) {
            Some(Node::Dir { entries, .. }) => entries,
            _ => {
                return Err(Error::Unsupported(format!(
                    "{path} is not under a directory"
                )));
            }
        };
    }
    entries.insert(name.to_owned(), node);
    Ok(())
}

/// Build `tree` as EROFS in `region`, LZ4HC-compressed into 32 KiB pclusters: file
/// contents are read from `store` as they are written. Returns the image's size.
pub(super) fn write(
    store: &Store<'_>,
    tree: &PartitionTree,
    region: Region,
    uuid: [u8; 16],
    epoch: u64,
) -> Result<u64, Error> {
    let mut root = BTreeMap::new();
    // Entries are sorted, so every directory is added before what it holds.
    for (path, entry) in &tree.entries {
        let mode = entry.metadata.mode;
        let node = match &entry.kind {
            EntryKind::Directory => Node::Dir {
                mode: 0o040000 | mode,
                entries: BTreeMap::new(),
                meta: meta(&entry.metadata),
                xattrs: xattrs(&entry.metadata)?,
            },
            EntryKind::File { content, size, .. } => Node::StreamedFile {
                mode: 0o100000 | mode,
                content: Content::new(*size, move || {
                    store.open(content, *size).map_err(io::Error::other)
                }),
                meta: meta(&entry.metadata),
                xattrs: xattrs(&entry.metadata)?,
            },
            EntryKind::Symlink { target } => Node::Symlink {
                mode: 0o120000 | mode,
                target: String::from_utf8(target.clone()).map_err(|_| {
                    Error::Unsupported(format!("non-UTF-8 symlink target at {path}"))
                })?,
                meta: meta(&entry.metadata),
                xattrs: xattrs(&entry.metadata)?,
            },
            EntryKind::Hardlink { target } => Node::HardLink {
                target: format!("/{target}"),
            },
        };
        insert(&mut root, path.as_ref(), node)?;
    }
    let root = Node::Dir {
        mode: 0o040000 | tree.root.mode,
        entries: root,
        meta: meta(&tree.root),
        xattrs: xattrs(&tree.root)?,
    };
    let options = BuildOptions {
        uuid,
        build_time: epoch,
        mtime_is_build_time: true,
        share_xattrs: true,
        compression: Some(Compression::LZ4HC_12_32K),
        ..BuildOptions::default()
    };
    let mut sink = BufWriter::new(region);
    let size = mkfs::build_image_to(root, 12, options, &mut sink)?;
    sink.flush()?;
    Ok(size)
}
