//! Bytes overwritten at offsets of a file.

use recipe::Hex;
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("binary edit at {0} is outside the input")]
    Bounds(u64),
}

pub fn apply(input: &[u8], edits: &BTreeMap<u64, Hex>) -> Result<Vec<u8>, Error> {
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
