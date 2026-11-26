use crate::metadata::AndroidMetadata;
use crate::{EntryKind, PartitionTree};
use crate::{Error, Region, Store, mode};
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

/// Images are built in 4 KiB blocks.
const BLOCK_BITS: u8 = 12;

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

/// Build `tree` as EROFS in `region`: a file unchanged in an EROFS source keeps the
/// pclusters that image stores it in, and every other is LZ4HC-compressed into 32 KiB
/// pclusters, its content read from `store` as it is written. Returns the image's size.
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
                mode: mode::DIRECTORY | mode,
                entries: BTreeMap::new(),
                meta: meta(&entry.metadata),
                xattrs: xattrs(&entry.metadata)?,
            },
            // A file unchanged in an EROFS source is not compressed again: its
            // pclusters are copied as that image stores them.
            EntryKind::File { content, size, .. } => match store
                .transplant(content)?
                .filter(|data| data.blkszbits() == BLOCK_BITS && data.size() == *size)
            {
                Some(data) => Node::TransplantedFile {
                    mode: mode::FILE | mode,
                    data,
                    meta: meta(&entry.metadata),
                    xattrs: xattrs(&entry.metadata)?,
                },
                None => Node::StreamedFile {
                    mode: mode::FILE | mode,
                    content: Content::new(*size, move || {
                        store.open(content, *size).map_err(io::Error::other)
                    }),
                    meta: meta(&entry.metadata),
                    xattrs: xattrs(&entry.metadata)?,
                },
            },
            EntryKind::Symlink { target } => Node::Symlink {
                mode: mode::SYMLINK | mode,
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
        mode: mode::DIRECTORY | tree.root.mode,
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
    // The image reaches the file in large writes rather than one a pcluster.
    let mut sink = BufWriter::with_capacity(1 << 20, region);
    let size = mkfs::build_image_to(root, BLOCK_BITS, options, &mut sink)?;
    sink.flush().map_err(Error::Write)?;
    Ok(size)
}
