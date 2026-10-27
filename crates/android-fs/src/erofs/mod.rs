//! A read-only EROFS reader covering what Android images use: compact and extended
//! inodes, flat and tail-inlined data, LZ4 compression with compact or full indexes
//! (including big pclusters and 0padding), and inline or shared xattrs.
//!
//! The on-disk format is documented at <https://erofs.docs.kernel.org>. Images using
//! anything else (chunked files, ztailpacking, fragments, other algorithms, long
//! xattr prefixes) are rejected rather than misread.

mod lz4;
mod zmap;

use crate::device::into_io;
use fs_core::BlockRead;
use std::{
    collections::BTreeMap,
    io::{self, Read},
};

const MAGIC: u32 = 0xE0F5_E1E2;
const SUPERBLOCK_OFFSET: u64 = 1024;

fn unsupported(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!("EROFS: unsupported {what}"),
    )
}

fn corrupt(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("EROFS: corrupt {what}"))
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

pub struct Erofs<D> {
    device: D,
    blkszbits: u8,
    meta_blkaddr: u64,
    xattr_blkaddr: u64,
    root_nid: u64,
    zero_padding: bool,
}

/// An inode as stored on disk. `mode` carries the file type and permission bits.
#[derive(Clone, Debug)]
pub struct Inode {
    pub nid: u64,
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
    pub nlink: u32,
    pub size: u64,
    format: u16,
    /// Byte offset of the inode.
    location: u64,
    inode_size: u64,
    xattr_size: u64,
    xattr_count: u16,
    raw_blkaddr: u32,
}

impl Inode {
    pub fn is_dir(&self) -> bool {
        self.mode & 0o170000 == 0o040000
    }

    pub fn is_file(&self) -> bool {
        self.mode & 0o170000 == 0o100000
    }

    pub fn is_symlink(&self) -> bool {
        self.mode & 0o170000 == 0o120000
    }

    fn layout(&self) -> u16 {
        (self.format >> 1) & 7
    }

    /// End of the inode and its xattrs: where inline data or the compression map starts.
    fn body_end(&self) -> u64 {
        self.location + self.inode_size + self.xattr_size
    }
}

impl<D: BlockRead> Erofs<D> {
    pub fn open(device: D) -> io::Result<Self> {
        let mut sb = [0u8; 128];
        device
            .read_at(SUPERBLOCK_OFFSET, &mut sb)
            .map_err(into_io)?;
        if u32_at(&sb, 0) != MAGIC {
            return Err(corrupt("superblock magic"));
        }
        let blkszbits = sb[12];
        if !(9..=16).contains(&blkszbits) {
            return Err(unsupported("block size"));
        }
        let incompat = u32_at(&sb, 80);
        // Known-safe incompat bits: 0padding, compr_cfgs, big_pcluster.
        const ZERO_PADDING: u32 = 0x1;
        const COMPR_CFGS: u32 = 0x2;
        const BIG_PCLUSTER: u32 = 0x2;
        if incompat & !(ZERO_PADDING | COMPR_CFGS | BIG_PCLUSTER) != 0 {
            return Err(unsupported(&format!("features {incompat:#x}")));
        }
        Ok(Self {
            device,
            blkszbits,
            meta_blkaddr: u32_at(&sb, 40) as u64,
            xattr_blkaddr: u32_at(&sb, 44) as u64,
            root_nid: u16_at(&sb, 14) as u64,
            zero_padding: incompat & ZERO_PADDING != 0,
        })
    }

    pub fn block_size(&self) -> u64 {
        1 << self.blkszbits
    }

    pub fn root(&self) -> u64 {
        self.root_nid
    }

    fn read(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let mut buf = vec![0; len];
        self.device.read_at(offset, &mut buf).map_err(into_io)?;
        Ok(buf)
    }

    pub fn inode(&self, nid: u64) -> io::Result<Inode> {
        let location = (self.meta_blkaddr << self.blkszbits) + nid * 32;
        let raw = self.read(location, 64)?;
        let format = u16_at(&raw, 0);
        let xattr_count = u16_at(&raw, 2);
        let xattr_size = if xattr_count == 0 {
            0
        } else {
            12 + (xattr_count as u64 - 1) * 4
        };
        let mode = u16_at(&raw, 4);
        let inode = if format & 1 == 0 {
            Inode {
                nid,
                mode,
                nlink: u16_at(&raw, 6) as u32,
                size: u32_at(&raw, 8) as u64,
                raw_blkaddr: u32_at(&raw, 16),
                uid: u16_at(&raw, 24) as u32,
                gid: u16_at(&raw, 26) as u32,
                format,
                location,
                inode_size: 32,
                xattr_size,
                xattr_count,
            }
        } else {
            Inode {
                nid,
                mode,
                size: u64_at(&raw, 8),
                raw_blkaddr: u32_at(&raw, 16),
                uid: u32_at(&raw, 24),
                gid: u32_at(&raw, 28),
                nlink: u32_at(&raw, 44),
                format,
                location,
                inode_size: 64,
                xattr_size,
                xattr_count,
            }
        };
        Ok(inode)
    }

    /// Extended attributes, shared ones first, as full names (`security.selinux`).
    pub fn xattrs(&self, inode: &Inode) -> io::Result<BTreeMap<String, Vec<u8>>> {
        let mut xattrs = BTreeMap::new();
        if inode.xattr_count == 0 {
            return Ok(xattrs);
        }
        let start = inode.location + inode.inode_size;
        let body = self.read(start, inode.xattr_size as usize)?;
        let shared = body[4] as usize;
        for i in 0..shared {
            let id = u32_at(&body, 12 + i * 4) as u64;
            let at = (self.xattr_blkaddr << self.blkszbits) + id * 4;
            let header = self.read(at, 4)?;
            let entry = self.read(at, 4 + header[0] as usize + u16_at(&header, 2) as usize)?;
            let (name, value) = entry_of(&entry)?;
            xattrs.insert(name, value);
        }
        let mut at = 12 + shared * 4;
        while at + 4 <= body.len() {
            let (name, value) = entry_of(&body[at..])?;
            let len = 4 + body[at] as usize + u16_at(&body, at + 2) as usize;
            at += len.div_ceil(4) * 4;
            xattrs.insert(name, value);
        }
        Ok(xattrs)
    }

    /// Directory entries other than `.` and `..`, as (name, nid).
    pub fn read_dir(&self, inode: &Inode) -> io::Result<Vec<(Vec<u8>, u64)>> {
        let data = self.read_all(inode)?;
        let block = self.block_size() as usize;
        let mut entries = Vec::new();
        for chunk in data.chunks(block) {
            if chunk.len() < 12 {
                return Err(corrupt("directory block"));
            }
            let count = u16_at(chunk, 8) as usize / 12;
            for i in 0..count {
                let dirent = &chunk[i * 12..i * 12 + 12];
                let start = u16_at(dirent, 8) as usize;
                let end = if i + 1 < count {
                    u16_at(chunk, (i + 1) * 12 + 8) as usize
                } else {
                    chunk.len()
                };
                let mut name = chunk.get(start..end).ok_or_else(|| corrupt("dirent"))?;
                if let Some(nul) = name.iter().position(|b| *b == 0) {
                    name = &name[..nul];
                }
                if name != b"." && name != b".." {
                    entries.push((name.to_vec(), u64_at(dirent, 0)));
                }
            }
        }
        Ok(entries)
    }

    pub fn read_all(&self, inode: &Inode) -> io::Result<Vec<u8>> {
        let mut data = Vec::with_capacity(inode.size as usize);
        self.contents(inode)?.read_to_end(&mut data)?;
        Ok(data)
    }

    /// Stream a file's contents.
    pub fn contents<'a>(&'a self, inode: &Inode) -> io::Result<Contents<'a, D>> {
        let extents = match inode.layout() {
            0 | 2 => self.flat_extents(inode),
            1 | 3 => zmap::extents(self, inode)?,
            4 => return Err(unsupported("chunk-based files")),
            _ => return Err(corrupt("data layout")),
        };
        Ok(Contents {
            fs: self,
            extents,
            next: 0,
            buffer: Vec::new(),
            position: 0,
        })
    }

    fn flat_extents(&self, inode: &Inode) -> Vec<Extent> {
        let block = self.block_size();
        let blocks = inode.size.div_ceil(block);
        let inline = inode.layout() == 2 && blocks > 0;
        let plain_len = if inline {
            (blocks - 1) * block
        } else {
            inode.size
        };
        let mut extents = Vec::new();
        if plain_len > 0 {
            extents.push(Extent::Raw {
                offset: (inode.raw_blkaddr as u64) << self.blkszbits,
                len: plain_len,
            });
        }
        if inline {
            extents.push(Extent::Raw {
                offset: inode.body_end(),
                len: inode.size - plain_len,
            });
        }
        extents
    }
}

fn entry_of(entry: &[u8]) -> io::Result<(String, Vec<u8>)> {
    let (name_len, index, value_len) = (entry[0] as usize, entry[1], u16_at(entry, 2) as usize);
    let prefix = match index {
        1 => "user.",
        2 => "system.posix_acl_access",
        3 => "system.posix_acl_default",
        4 => "trusted.",
        6 => "security.",
        _ => return Err(unsupported(&format!("xattr name index {index}"))),
    };
    let name = entry.get(4..4 + name_len).ok_or_else(|| corrupt("xattr"))?;
    let value = entry
        .get(4 + name_len..4 + name_len + value_len)
        .ok_or_else(|| corrupt("xattr"))?;
    Ok((
        format!("{prefix}{}", String::from_utf8_lossy(name)),
        value.to_vec(),
    ))
}

/// A contiguous piece of a file's contents, in logical order.
enum Extent {
    /// Stored bytes at a device offset.
    Raw { offset: u64, len: u64 },
    /// An LZ4 pcluster of `blocks` blocks at `offset`, decoding to `len` bytes.
    Lz4 { offset: u64, blocks: u64, len: u64 },
}

/// A file's contents as a stream, decoding one extent at a time.
pub struct Contents<'a, D> {
    fs: &'a Erofs<D>,
    extents: Vec<Extent>,
    next: usize,
    buffer: Vec<u8>,
    position: usize,
}

impl<D: BlockRead> Read for Contents<'_, D> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.position == self.buffer.len() {
            let Some(extent) = self.extents.get_mut(self.next) else {
                return Ok(0);
            };
            self.position = 0;
            self.buffer = match *extent {
                // Stored bytes are read a bounded piece at a time; large files are flat.
                Extent::Raw { offset, len } => {
                    let take = len.min(1 << 20);
                    if take < len {
                        *extent = Extent::Raw {
                            offset: offset + take,
                            len: len - take,
                        };
                    } else {
                        self.next += 1;
                    }
                    self.fs.read(offset, take as usize)?
                }
                Extent::Lz4 {
                    offset,
                    blocks,
                    len,
                } => {
                    let compressed = self
                        .fs
                        .read(offset, (blocks << self.fs.blkszbits) as usize)?;
                    let input = if self.fs.zero_padding {
                        let skip = compressed.iter().take_while(|b| **b == 0).count();
                        &compressed[skip..]
                    } else {
                        &compressed[..]
                    };
                    let mut output = vec![0; len as usize];
                    lz4::decompress(input, &mut output)?;
                    self.next += 1;
                    output
                }
            };
        }
        let count = out.len().min(self.buffer.len() - self.position);
        out[..count].copy_from_slice(&self.buffer[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}
