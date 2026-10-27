//! Compressed file layout: from logical clusters (lclusters) to pclusters.
//!
//! A compressed inode is cut into lclusters of `1 << lclusterbits` bytes, each with
//! an index entry. A HEAD (or uncompressed PLAIN) entry starts a pcluster at
//! `clusterofs` within its lcluster; NONHEAD entries continue the previous one. With
//! big pclusters, the first NONHEAD after a HEAD records the pcluster's compressed
//! block count (CBLKCNT). Indexes are either "full" (8 bytes per lcluster) or
//! "compact" (packs of 2 or 16 entries sharing one base block address).

use super::{Erofs, Extent, Inode, corrupt, u16_at, u32_at, unsupported};
use fs_core::BlockRead;
use std::io;

const PLAIN: u8 = 0;
const HEAD1: u8 = 1;
const NONHEAD: u8 = 2;
const HEAD2: u8 = 3;

const CBLKCNT: u32 = 1 << 11;
const ADVISE_COMPACTED_2B: u16 = 0x1;
const ADVISE_BIG_PCLUSTER_1: u16 = 0x2;
const ADVISE_BIG_PCLUSTER_2: u16 = 0x4;
const ADVISE_INTERLACED: u16 = 0x10;
const ADVISE_INLINE_PCLUSTER: u16 = 0x8;
const ADVISE_FRAGMENT: u16 = 0x20;

/// One lcluster's index entry, decoded.
struct Lcluster {
    kind: u8,
    /// HEAD/PLAIN: where the pcluster starts within the lcluster.
    clusterofs: u32,
    /// HEAD/PLAIN: the pcluster's first block.
    pblk: u64,
    /// NONHEAD carrying a CBLKCNT marker: the pcluster's compressed block count.
    blocks: Option<u64>,
}

struct Map {
    index: Vec<u8>,
    index_start: u64,
    lclusterbits: u32,
    big_pcluster: bool,
    compacted_2b: bool,
    total: u64,
}

pub(super) fn extents<D: BlockRead>(fs: &Erofs<D>, inode: &Inode) -> io::Result<Vec<Extent>> {
    if inode.size == 0 {
        return Ok(Vec::new());
    }
    let header_at = inode.body_end().next_multiple_of(8);
    let header = fs.read(header_at, 8)?;
    let advise = u16_at(&header, 4);
    if advise & (ADVISE_INTERLACED | ADVISE_INLINE_PCLUSTER | ADVISE_FRAGMENT) != 0 {
        return Err(unsupported(&format!("compression advise {advise:#x}")));
    }
    if header[6] != 0 {
        return Err(unsupported("compression algorithm other than LZ4"));
    }
    let lclusterbits = fs.blkszbits as u32 + (header[7] & 7) as u32;
    let total = inode.size.div_ceil(1 << lclusterbits);
    let full = inode.layout() == 1;
    let (index_start, index_len) = if full {
        (header_at + 16, total * 8)
    } else {
        // Generous upper bound: every entry in 4-byte form plus pack alignment.
        (header_at + 8, total * 4 + 32)
    };
    let map = Map {
        index: fs.read(index_start, index_len as usize)?,
        index_start,
        lclusterbits,
        big_pcluster: advise & (ADVISE_BIG_PCLUSTER_1 | ADVISE_BIG_PCLUSTER_2) != 0,
        compacted_2b: advise & ADVISE_COMPACTED_2B != 0,
        total,
    };
    let load = |lcn| {
        if full {
            map.full(lcn)
        } else {
            map.compact(lcn)
        }
    };

    // Pcluster heads in order: (logical start, lcn, entry).
    let mut heads = Vec::new();
    for lcn in 0..total {
        let entry = load(lcn)?;
        if entry.kind != NONHEAD {
            let start = (lcn << lclusterbits) + entry.clusterofs as u64;
            if start < inode.size {
                heads.push((start, lcn, entry));
            }
        }
    }
    let mut extents = Vec::with_capacity(heads.len());
    for (i, (start, lcn, entry)) in heads.iter().enumerate() {
        let end = heads.get(i + 1).map_or(inode.size, |next| next.0);
        let len = end - start;
        let offset = entry.pblk << fs.blkszbits;
        match entry.kind {
            PLAIN => extents.push(Extent::Raw { offset, len }),
            HEAD1 | HEAD2 => {
                let blocks = match lcn + 1 < total && map.big_pcluster {
                    true => load(lcn + 1)?.blocks.unwrap_or(1),
                    false => 1,
                };
                extents.push(Extent::Lz4 {
                    offset,
                    blocks,
                    len,
                });
            }
            _ => unreachable!(),
        }
    }
    Ok(extents)
}

impl Map {
    fn full(&self, lcn: u64) -> io::Result<Lcluster> {
        let at = (lcn * 8) as usize;
        let entry = self
            .index
            .get(at..at + 8)
            .ok_or_else(|| corrupt("full index"))?;
        let kind = (u16_at(entry, 0) & 3) as u8;
        if kind == NONHEAD {
            let delta0 = u16_at(entry, 4) as u32;
            return Ok(Lcluster {
                kind,
                clusterofs: 0,
                pblk: 0,
                blocks: (delta0 & CBLKCNT != 0).then_some((delta0 & (CBLKCNT - 1)) as u64),
            });
        }
        Ok(Lcluster {
            kind,
            clusterofs: u16_at(entry, 2) as u32,
            pblk: u32_at(entry, 4) as u64,
            blocks: None,
        })
    }

    /// Byte position (relative to the index start) and entry width of `lcn`.
    fn compact_position(&self, lcn: u64) -> (u64, u32) {
        let ebase = self.index_start;
        let mut initial_4b = (32 - ebase % 32) / 4;
        if initial_4b == 8 {
            initial_4b = 0;
        }
        let compacted_2b = if self.compacted_2b && initial_4b < self.total {
            (self.total - initial_4b) / 16 * 16
        } else {
            0
        };
        if lcn < initial_4b {
            return (lcn * 4, 2);
        }
        let mut pos = initial_4b * 4;
        let lcn = lcn - initial_4b;
        if lcn < compacted_2b {
            return (pos + lcn * 2, 1);
        }
        pos += compacted_2b * 2;
        (pos + (lcn - compacted_2b) * 4, 2)
    }

    fn compact(&self, lcn: u64) -> io::Result<Lcluster> {
        if self.lclusterbits > 12 && self.compacted_2b {
            return Err(unsupported("compact 2B index with lclusters over 4 KiB"));
        }
        let (pos, amortizedshift) = self.compact_position(lcn);
        let vcnt: u64 = if amortizedshift == 2 { 2 } else { 16 };
        let pack_len = vcnt << amortizedshift;
        // Packs are aligned relative to the device, not the index start.
        let absolute = self.index_start + pos;
        let pack_at = absolute - absolute % pack_len - self.index_start;
        let pack = self
            .index
            .get(pack_at as usize..(pack_at + pack_len) as usize)
            .ok_or_else(|| corrupt("compact index"))?;
        let lobits = self.lclusterbits.max(12);
        let encodebits = ((pack_len - 4) * 8 / vcnt) as u32;
        let mut i = ((absolute % pack_len) >> amortizedshift) as i64;
        let decode = |i: i64| -> (u32, u8) {
            let bit = encodebits as usize * i as usize;
            let mut word = [0u8; 4];
            let available = &pack[bit / 8..(bit / 8 + 4).min(pack.len())];
            word[..available.len()].copy_from_slice(available);
            let v = u32::from_le_bytes(word) >> (bit % 8);
            (v & ((1 << lobits) - 1), ((v >> lobits) & 3) as u8)
        };
        let (lo, kind) = decode(i);
        if kind == NONHEAD {
            return Ok(Lcluster {
                kind,
                clusterofs: 0,
                pblk: 0,
                blocks: (lo & CBLKCNT != 0).then_some((lo & (CBLKCNT - 1)) as u64),
            });
        }
        // Blocks used by the pcluster heads before this one in the pack.
        let mut nblk: u64 = if self.big_pcluster { 0 } else { 1 };
        while i > 0 {
            i -= 1;
            let (lo, kind) = decode(i);
            if self.big_pcluster {
                if kind == NONHEAD {
                    if lo & CBLKCNT != 0 {
                        i -= 1;
                        nblk += (lo & (CBLKCNT - 1)) as u64;
                        continue;
                    }
                    if lo <= 1 {
                        return Err(corrupt("big pcluster delta"));
                    }
                    i -= lo as i64 - 2;
                    continue;
                }
                nblk += 1;
            } else {
                if kind == NONHEAD {
                    i -= lo as i64;
                }
                if i >= 0 {
                    nblk += 1;
                }
            }
        }
        let base = u32_at(pack, pack.len() - 4) as u64;
        Ok(Lcluster {
            kind,
            clusterofs: lo,
            pblk: base + nblk,
            blocks: None,
        })
    }
}
