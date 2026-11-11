//! Content hashes: SHA-256 digests, and bytes written as hex.

use crate::Error;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    io::{self, Read, Write},
};

/// Bytes as lowercase hex, two digits each.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The bytes `text` writes as hex, two digits each, or `None` if it is not that.
pub fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).ok())
        .collect()
}

/// A SHA-256 digest, written as 64 lowercase hex digits.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Sha256Digest([u8; 32]);

impl TryFrom<String> for Sha256Digest {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Error> {
        unhex(&value)
            .filter(|_| !value.bytes().any(|b| b.is_ascii_uppercase()))
            .and_then(|bytes| bytes.try_into().ok())
            .map(Self)
            .ok_or(Error::Invalid {
                kind: "SHA-256 digest",
                value,
            })
    }
}

impl From<Sha256Digest> for String {
    fn from(value: Sha256Digest) -> Self {
        hex(&value.0)
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex(&self.0))
    }
}

impl fmt::Debug for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sha256Digest({self})")
    }
}

impl Sha256Digest {
    pub fn bytes(bytes: &[u8]) -> Self {
        Self::finish(Sha256::new_with_prefix(bytes))
    }

    pub fn reader(mut reader: impl Read) -> io::Result<Self> {
        let mut writer = Sha256Writer::new(io::sink());
        io::copy(&mut reader, &mut writer)?;
        Ok(writer.finish()?.1)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    fn finish(digest: Sha256) -> Self {
        Self(digest.finalize().into())
    }
}

/// Writer that hashes everything it passes through to `inner`.
struct Sha256Writer<W> {
    inner: W,
    digest: Sha256,
}

impl<W: Write> Sha256Writer<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            digest: Sha256::new(),
        }
    }

    fn finish(mut self) -> io::Result<(W, Sha256Digest)> {
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
