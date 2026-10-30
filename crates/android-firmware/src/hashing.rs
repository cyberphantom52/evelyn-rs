use crate::Sha256Digest;
use sha2::{Digest, Sha256};
use std::io::{self, Write};

/// Writer that hashes everything it passes through to `inner`.
pub struct Sha256Writer<W> {
    inner: W,
    digest: Sha256,
}

impl<W: Write> Sha256Writer<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            digest: Sha256::new(),
        }
    }

    pub fn finish(mut self) -> io::Result<(W, Sha256Digest)> {
        self.inner.flush()?;
        Ok((self.inner, Sha256Digest::finish(self.digest)))
    }
}

impl<W: Write> Write for Sha256Writer<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self.inner.write(bytes)?;
        self.digest.update(&bytes[..count]);
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
