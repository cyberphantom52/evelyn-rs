//! Fixed-width byte edits checked against their expected preimage.
//! Recipe validation guarantees edits are nonempty and equal-width.

use model::Sha256Digest;
use recipe::ByteEdit;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("binary input SHA-256 mismatch")]
    HashMismatch,
    #[error("binary edit {0} is outside the input")]
    Bounds(usize),
    #[error("binary edit {0} preimage mismatch")]
    Preimage(usize),
}

pub fn apply(
    input: &[u8],
    before: Option<&Sha256Digest>,
    edits: &[ByteEdit],
) -> Result<Vec<u8>, Error> {
    if before.is_some_and(|before| Sha256Digest::bytes(input) != *before) {
        return Err(Error::HashMismatch);
    }
    let mut output = input.to_vec();
    for (index, edit) in edits.iter().enumerate() {
        let start = usize::try_from(edit.offset).map_err(|_| Error::Bounds(index))?;
        let range = start..start + edit.expected.len();
        match input.get(range.clone()) {
            None => return Err(Error::Bounds(index)),
            Some(preimage) if preimage != edit.expected => return Err(Error::Preimage(index)),
            Some(_) => output[range].copy_from_slice(&edit.replacement),
        }
    }
    Ok(output)
}
