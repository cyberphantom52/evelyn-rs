//! Unified diffs applied to a file whose exact content is pinned by its SHA-256.

use android_firmware::Sha256Digest;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("text input SHA-256 mismatch")]
    HashMismatch,
    #[error("invalid unified diff: {0}")]
    InvalidPatch(String),
    #[error("diff does not apply: {0}")]
    Apply(String),
}

pub fn apply(input: &str, before: Option<&Sha256Digest>, patch: &str) -> Result<String, Error> {
    if before.is_some_and(|before| Sha256Digest::bytes(input.as_bytes()) != *before) {
        return Err(Error::HashMismatch);
    }
    let patch = diffy::Patch::from_str(patch).map_err(|e| Error::InvalidPatch(e.to_string()))?;
    diffy::apply(input, &patch).map_err(|e| Error::Apply(e.to_string()))
}
