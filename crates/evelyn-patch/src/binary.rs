//! Bytes overwritten at offsets of a file whose whole content is checked first.

use android_firmware::Sha256Digest;
use recipe::Hex;
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("binary input SHA-256 mismatch")]
    HashMismatch,
    #[error("binary edit at {0} is outside the input")]
    Bounds(u64),
}

pub fn apply(
    input: &[u8],
    before: &Sha256Digest,
    edits: &BTreeMap<u64, Hex>,
) -> Result<Vec<u8>, Error> {
    if Sha256Digest::bytes(input) != *before {
        return Err(Error::HashMismatch);
    }
    let mut output = input.to_vec();
    for (offset, Hex(bytes)) in edits {
        let start = usize::try_from(*offset).map_err(|_| Error::Bounds(*offset))?;
        output
            .get_mut(start..start + bytes.len())
            .ok_or(Error::Bounds(*offset))?
            .copy_from_slice(bytes);
    }
    Ok(output)
}
