use crate::{EntryKind, PartitionTree};
use crate::{Error, Store};
use android_firmware::AndroidMetadata;
use std::{
    fs,
    io::Read,
    os::unix::fs::FileExt,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

const BLOCK_SIZE: u64 = 4096;

/// The image file as a block device. It is a scratch output that is rebuilt from
/// scratch after any failure, so writes are never synced while populating.
struct ImageDevice {
    file: fs::File,
    size: u64,
}

impl fs_core::BlockRead for ImageDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> fs_core::Result<()> {
        Ok(self.file.read_exact_at(buf, offset)?)
    }

    fn size_bytes(&self) -> u64 {
        self.size
    }
}

/// `flush` keeps its no-op default.
impl fs_core::BlockDevice for ImageDevice {
    fn write_at(&self, offset: u64, buf: &[u8]) -> fs_core::Result<()> {
        Ok(self.file.write_all_at(buf, offset)?)
    }

    fn is_writable(&self) -> bool {
        true
    }
}

struct FixedRuntime {
    epoch: i64,
    generation: AtomicU32,
}
impl fs_ext4::runtime::Runtime for FixedRuntime {
    fn now_unix_seconds(&self) -> i64 {
        self.epoch
    }
    fn next_inode_generation(&self) -> u32 {
        self.generation.fetch_add(1, Ordering::Relaxed)
    }
}

fn inode_for(fs: &fs_ext4::Filesystem, path: &str) -> Result<u32, Error> {
    let mut ino = 2;
    for component in path.split('/').filter(|component| !component.is_empty()) {
        ino = fs.lookup_at(ino, component.as_bytes())?;
    }
    Ok(ino)
}

fn metadata(
    fs: &fs_ext4::Filesystem,
    ino: u32,
    path: &str,
    value: &AndroidMetadata,
) -> Result<(), Error> {
    fs.apply_chown_ino(ino, value.uid, value.gid)?;
    for (name, value) in &value.xattrs {
        fs.apply_setxattr(path, name, value)?;
    }
    fs.apply_chmod_ino(ino, value.mode)?;
    Ok(())
}

/// Content blocks plus 10% and 4 MiB of headroom, like the AOSP build defaults,
/// and an inode for every entry plus 1000.
fn size(tree: &PartitionTree) -> (u64, u64) {
    let blocks: u64 = tree
        .entries
        .values()
        .map(|entry| match &entry.kind {
            EntryKind::File { size, .. } => size.div_ceil(BLOCK_SIZE),
            EntryKind::Hardlink { .. } => 0,
            _ => 1,
        })
        .sum();
    (
        blocks + blocks / 10 + 1024,
        tree.entries.len() as u64 + 1000,
    )
}

/// Format a new ext4 image at `image`, without a journal, and populate it with `tree`.
pub fn write_ext4(
    store: &Store<'_>,
    tree: &PartitionTree,
    image: &Path,
    label: &str,
    uuid: [u8; 16],
    epoch: u64,
) -> Result<(), Error> {
    let (blocks, inodes) = size(tree);
    let inodes = u32::try_from(inodes)
        .map_err(|_| Error::Unsupported(format!("{inodes} inodes do not fit in ext4")))?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(image)?;
    file.set_len(blocks * BLOCK_SIZE)?;
    let device = Arc::new(fs_ext4::fs_core_bridge::CoreDevice::new(ImageDevice {
        size: blocks * BLOCK_SIZE,
        file,
    }));
    fs_ext4::mkfs::format_filesystem_with_options(
        &*device,
        Some(label),
        Some(uuid),
        blocks * BLOCK_SIZE,
        BLOCK_SIZE as u32,
        &fs_ext4::mkfs::FormatOptions {
            inodes_count: Some(inodes),
            ..fs_ext4::mkfs::FormatOptions::new(fs_ext4::features::FsFlavor::Ext4)
        },
    )?;
    let runtime = Arc::new(FixedRuntime {
        epoch: epoch as i64,
        generation: AtomicU32::new(1),
    });
    let fs = fs_ext4::Filesystem::mount_with_runtime(device, runtime)?;
    fs.apply_rmdir("/lost+found")?;
    for (path, entry) in &tree.entries {
        let absolute = format!("/{path}");
        let ino = match &entry.kind {
            EntryKind::Directory => fs.apply_mkdir(&absolute, entry.metadata.mode)?,
            EntryKind::File { content, size, .. } => {
                let ino = fs.apply_create(&absolute, entry.metadata.mode)?;
                let mut input = store.open(content, *size)?;
                let mut buffer = [0_u8; 64 * 1024];
                let mut offset = 0_u64;
                loop {
                    let count = input.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    fs.apply_pwrite_ino(ino, offset, &buffer[..count])?;
                    offset += count as u64;
                }
                ino
            }
            EntryKind::Symlink { target } => {
                let (parent, name) = path
                    .as_ref()
                    .rsplit_once('/')
                    .unwrap_or(("", path.as_ref()));
                fs.apply_symlink_at(inode_for(&fs, parent)?, name.as_bytes(), target)?
            }
            EntryKind::Hardlink { .. } => continue,
        };
        metadata(&fs, ino, &absolute, &entry.metadata)?;
    }
    for (path, entry) in &tree.entries {
        if let EntryKind::Hardlink { target } = &entry.kind {
            fs.apply_link(&format!("/{target}"), &format!("/{path}"))?;
        }
    }
    metadata(&fs, 2, "/", &tree.root)?;
    fs.flush()?;
    // The formatter leaves off a last block group too small for its metadata, as
    // mke2fs does, so the volume can end before the file.
    let formatted = fs.sb.blocks_count * BLOCK_SIZE;
    drop(fs);
    fs::OpenOptions::new()
        .write(true)
        .open(image)?
        .set_len(formatted)?;
    Ok(())
}
