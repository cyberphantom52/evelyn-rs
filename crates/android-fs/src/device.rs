//! Image files as random-access devices, with Android sparse images expanded on the fly.

use fs_core::{BlockDevice, BlockRead, FileDevice};
use std::{io, path::Path};

const SPARSE_MAGIC: u32 = 0xED26_FF3A;
const RAW: u16 = 0xCAC1;
const FILL: u16 = 0xCAC2;
const DONT_CARE: u16 = 0xCAC3;
const CRC32: u16 = 0xCAC4;

/// A raw image, or a sparse image read as the raw image it describes.
pub enum Device {
    Raw(FileDevice),
    Sparse(Sparse),
}

impl Device {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = FileDevice::open(path).map_err(into_io)?;
        let mut magic = [0u8; 4];
        if file.size_bytes() >= 28 {
            file.read_at(0, &mut magic).map_err(into_io)?;
        }
        if u32::from_le_bytes(magic) == SPARSE_MAGIC {
            Ok(Self::Sparse(Sparse::open(file)?))
        } else {
            Ok(Self::Raw(file))
        }
    }
}

impl BlockRead for Device {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> fs_core::Result<()> {
        match self {
            Self::Raw(file) => file.read_at(offset, buf),
            Self::Sparse(sparse) => sparse.read_at(offset, buf),
        }
    }

    fn size_bytes(&self) -> u64 {
        match self {
            Self::Raw(file) => file.size_bytes(),
            Self::Sparse(sparse) => sparse.len,
        }
    }
}

/// Read-only: the defaults refuse writes.
impl BlockDevice for Device {}

/// A device error as an I/O error, unwrapping plain I/O failures.
pub(crate) fn into_io(error: fs_core::Error) -> io::Error {
    match error {
        fs_core::Error::Io(error) => error,
        error => io::Error::new(io::ErrorKind::InvalidData, error.to_string()),
    }
}

enum Chunk {
    Raw(u64),
    Fill([u8; 4]),
    Zero,
}

/// An Android sparse image: (output offset, length, chunk), in output order.
pub struct Sparse {
    file: FileDevice,
    chunks: Vec<(u64, u64, Chunk)>,
    len: u64,
}

fn corrupt() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "malformed sparse image")
}

impl Sparse {
    fn open(file: FileDevice) -> io::Result<Self> {
        let mut header = [0u8; 28];
        file.read_at(0, &mut header).map_err(into_io)?;
        let u16_at = |at: usize| u16::from_le_bytes([header[at], header[at + 1]]);
        let u32_at = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
        let (header_len, chunk_header_len) = (u16_at(8) as u64, u16_at(10) as u64);
        let block = u32_at(12) as u64;
        let (total_chunks, mut at, mut out) = (u32_at(20), header_len, 0u64);
        let mut chunks = Vec::with_capacity(total_chunks as usize);
        for _ in 0..total_chunks {
            let mut chunk = [0u8; 12];
            file.read_at(at, &mut chunk).map_err(into_io)?;
            let kind = u16::from_le_bytes([chunk[0], chunk[1]]);
            let blocks = u32::from_le_bytes(chunk[4..8].try_into().unwrap()) as u64;
            let total = u32::from_le_bytes(chunk[8..12].try_into().unwrap()) as u64;
            let data = at + chunk_header_len;
            let len = blocks * block;
            match kind {
                RAW => chunks.push((out, len, Chunk::Raw(data))),
                FILL => {
                    let mut fill = [0u8; 4];
                    file.read_at(data, &mut fill).map_err(into_io)?;
                    chunks.push((out, len, Chunk::Fill(fill)));
                }
                DONT_CARE => chunks.push((out, len, Chunk::Zero)),
                CRC32 => {}
                _ => return Err(corrupt()),
            }
            out += len;
            at += total;
        }
        Ok(Self {
            file,
            chunks,
            len: out,
        })
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> fs_core::Result<()> {
        let mut done = 0;
        while done < buf.len() {
            let position = offset + done as u64;
            let index = self
                .chunks
                .partition_point(|(start, _, _)| *start <= position)
                .checked_sub(1)
                .ok_or_else(corrupt)?;
            let (start, len, chunk) = &self.chunks[index];
            let within = position - start;
            if within >= *len {
                return Err(fs_core::Error::ShortRead {
                    offset,
                    want: buf.len(),
                    got: done,
                });
            }
            let take = ((len - within) as usize).min(buf.len() - done);
            let out = &mut buf[done..done + take];
            match chunk {
                Chunk::Raw(data) => self.file.read_at(data + within, out)?,
                Chunk::Fill(fill) => {
                    for (i, byte) in out.iter_mut().enumerate() {
                        *byte = fill[(within as usize + i) % 4];
                    }
                }
                Chunk::Zero => out.fill(0),
            }
            done += take;
        }
        Ok(())
    }
}
