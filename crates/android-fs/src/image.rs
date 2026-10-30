//! Read-only access to EROFS and ext4 images.

use crate::device::{Device, into_io};
use crate::{Error, ImageFormat};
use android_firmware::{AndroidMetadata, Filesystem};
use fs_core::BlockRead;
use fs_ext4::fs_core_bridge::CoreDevice;
use std::{
    collections::BTreeMap,
    io::{self, Read},
    path::Path,
    sync::Arc,
};

enum Backend {
    Erofs(Box<fs_erofs::Filesystem>),
    Ext4(Box<fs_ext4::Filesystem>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
    Symlink,
}

pub struct Inode {
    pub kind: Kind,
    pub size: u64,
    pub links: u32,
    pub metadata: AndroidMetadata,
}

pub struct Image(Backend);

impl Image {
    /// Open an EROFS or ext4 image, raw or Android sparse, recognizing it by its
    /// superblock magic.
    pub fn detect(path: &Path) -> Result<Self, Error> {
        let device = Device::open(path)?;
        let mut prefix = [0u8; 1084];
        device.read_at(0, &mut prefix).map_err(into_io)?;
        Ok(Self(match ImageFormat::of(&prefix) {
            ImageFormat::Erofs => {
                Backend::Erofs(Box::new(fs_erofs::Filesystem::open(Arc::new(device))?))
            }
            ImageFormat::Ext4 => {
                let fs = fs_ext4::Filesystem::mount(Arc::new(CoreDevice::new(device)))?;
                // Without journal replay, a dirty filesystem would read stale data.
                if fs.sb.feature_incompat & 4 != 0 || fs.sb.state & 1 == 0 {
                    return Err(Error::Unsupported(
                        "ext4 source requires journal recovery or a clean filesystem".into(),
                    ));
                }
                Backend::Ext4(Box::new(fs))
            }
            _ => {
                return Err(Error::Unsupported(format!(
                    "{} is not EROFS or ext4",
                    path.display()
                )));
            }
        }))
    }

    /// Paths and contents of the regular files whose path satisfies `wanted`.
    pub fn read_files(
        &self,
        wanted: impl Fn(&str) -> bool,
    ) -> Result<Vec<(String, Vec<u8>)>, Error> {
        let mut found = Vec::new();
        let mut pending = vec![(String::new(), self.root()?)];
        while let Some((dir, id)) = pending.pop() {
            for (name, child) in self.children(id)? {
                let path = format!("{dir}{}", String::from_utf8_lossy(&name));
                let inode = self.inode(child)?;
                match inode.kind {
                    Kind::Directory => pending.push((format!("{path}/"), child)),
                    Kind::File if wanted(&path) => {
                        let mut data = Vec::with_capacity(inode.size as usize);
                        self.file(child, inode.size)?.read_to_end(&mut data)?;
                        found.push((path, data));
                    }
                    _ => {}
                }
            }
        }
        found.sort();
        Ok(found)
    }

    pub fn filesystem(&self) -> Filesystem {
        match &self.0 {
            Backend::Erofs(_) => Filesystem::Erofs,
            Backend::Ext4(_) => Filesystem::Ext4,
        }
    }

    pub fn root(&self) -> Result<u64, Error> {
        Ok(match &self.0 {
            Backend::Erofs(fs) => fs.root_inode()?.nid,
            Backend::Ext4(_) => 2,
        })
    }

    pub fn inode(&self, id: u64) -> Result<Inode, Error> {
        let (mode, uid, gid, size, links, xattrs) = match &self.0 {
            Backend::Erofs(fs) => {
                let inode = fs.read_inode(id)?;
                let xattrs = fs
                    .xattrs(&inode)?
                    .into_iter()
                    .map(|(name, value)| {
                        String::from_utf8(name)
                            .map(|name| (name, value))
                            .map_err(|_| {
                                Error::Unsupported(format!("non-UTF-8 xattr name on inode {id}"))
                            })
                    })
                    .collect::<Result<BTreeMap<_, _>, _>>()?;
                (
                    inode.mode,
                    inode.uid,
                    inode.gid,
                    inode.size,
                    inode.nlink,
                    xattrs,
                )
            }
            Backend::Ext4(fs) => {
                let (inode, raw) = fs.read_inode_verified(id as u32)?;
                let xattrs = fs_ext4::xattr::read_all_resolved(fs, &inode, &raw)?
                    .into_iter()
                    .map(|entry| (entry.name, entry.value))
                    .collect();
                (
                    inode.mode,
                    inode.uid,
                    inode.gid,
                    inode.size,
                    u32::from(inode.links_count),
                    xattrs,
                )
            }
        };
        let kind = match mode & 0o170000 {
            0o100000 => Kind::File,
            0o040000 => Kind::Directory,
            0o120000 => Kind::Symlink,
            _ => {
                return Err(Error::Unsupported(format!(
                    "special or unknown inode {id}, mode {mode:#o}"
                )));
            }
        };
        Ok(Inode {
            kind,
            size,
            links,
            metadata: AndroidMetadata {
                uid,
                gid,
                mode: mode & 0o7777,
                xattrs,
            },
        })
    }

    /// Directory entries other than `.` and `..`, sorted by name.
    pub fn children(&self, id: u64) -> Result<Vec<(Vec<u8>, u64)>, Error> {
        let mut result: Vec<_> = match &self.0 {
            Backend::Erofs(fs) => fs
                .read_dir(&fs.read_inode(id)?)?
                .into_iter()
                .map(|entry| (entry.name, entry.nid))
                .collect(),
            Backend::Ext4(fs) => fs
                .read_dir_ino(id as u32)?
                .into_iter()
                .map(|entry| (entry.name, u64::from(entry.inode)))
                .collect(),
        };
        result.retain(|(name, _)| name != b"." && name != b"..");
        result.sort();
        Ok(result)
    }

    pub fn link(&self, id: u64) -> Result<Vec<u8>, Error> {
        Ok(match &self.0 {
            Backend::Erofs(fs) => fs.read_symlink_target(&fs.read_inode(id)?)?,
            Backend::Ext4(fs) => fs.read_link_ino(id as u32)?,
        })
    }

    /// Stream the first `size` bytes of a regular file.
    pub fn file(&self, id: u64, size: u64) -> Result<Box<dyn Read + '_>, Error> {
        Ok(match &self.0 {
            Backend::Erofs(fs) => {
                let inode = fs.read_inode(id)?;
                Box::new(File::new(size, move |offset, output| {
                    fs.read_file(&inode, offset, output)?;
                    Ok(output.len())
                }))
            }
            Backend::Ext4(fs) => Box::new(File::new(size, move |offset, output| {
                Ok(fs.read_ino(id as u32, offset, output)?)
            })),
        })
    }
}

/// The first `size` bytes of a file, read from an image at increasing offsets.
struct File<F> {
    read_at: F,
    size: u64,
    offset: u64,
}

impl<F: FnMut(u64, &mut [u8]) -> Result<usize, Error>> File<F> {
    fn new(size: u64, read_at: F) -> Self {
        Self {
            read_at,
            size,
            offset: 0,
        }
    }
}

impl<F: FnMut(u64, &mut [u8]) -> Result<usize, Error>> Read for File<F> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = (self.size - self.offset).min(output.len() as u64) as usize;
        if count == 0 {
            return Ok(0);
        }
        let count = (self.read_at)(self.offset, &mut output[..count]).map_err(io::Error::other)?;
        self.offset += count as u64;
        Ok(count)
    }
}
