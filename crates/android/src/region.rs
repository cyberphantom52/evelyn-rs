//! Where a partition is built: a whole image file, or its place inside a `super` image.

use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::fs::FileExt,
};

/// The bytes of `file` from `offset` on, written at positions relative to `offset`.
/// It is a scratch output that is rebuilt after any failure, so writes are never synced.
pub struct Region {
    file: File,
    offset: u64,
    position: u64,
}

impl Region {
    pub fn new(file: File, offset: u64) -> Self {
        Self {
            file,
            offset,
            position: 0,
        }
    }

    /// Another handle on the same region, at its start.
    pub(crate) fn try_clone(&self) -> io::Result<Self> {
        Ok(Self::new(self.file.try_clone()?, self.offset))
    }

    /// Make the file reach at least `len` bytes into the region; new bytes read as zeros.
    pub(crate) fn reserve(&self, len: u64) -> io::Result<()> {
        let end = self.offset + len;
        if self.file.metadata()?.len() < end {
            self.file.set_len(end)?;
        }
        Ok(())
    }

    pub(crate) fn read_exact_at(&self, buf: &mut [u8], at: u64) -> io::Result<()> {
        self.file.read_exact_at(buf, self.offset + at)
    }

    pub(crate) fn write_all_at(&self, buf: &[u8], at: u64) -> io::Result<()> {
        self.file.write_all_at(buf, self.offset + at)
    }
}

impl Read for Region {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.file.read_at(buf, self.offset + self.position)?;
        self.position += read as u64;
        Ok(read)
    }
}

impl Write for Region {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_all_at(buf, self.position)?;
        self.position += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for Region {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let position = match to {
            SeekFrom::Start(at) => Some(at),
            SeekFrom::Current(by) => self.position.checked_add_signed(by),
            SeekFrom::End(by) => {
                (self.file.metadata()?.len().saturating_sub(self.offset)).checked_add_signed(by)
            }
        };
        self.position = position
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before the region"))?;
        Ok(self.position)
    }
}
