//! Unified diffs applied to a file.

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("invalid unified diff: {0}")]
    InvalidPatch(String),
    #[error("diff does not apply: {0}")]
    Apply(String),
}

pub fn apply(input: &str, patch: &str) -> Result<String, Error> {
    let patch = diffy::Patch::from_str(patch).map_err(|e| Error::InvalidPatch(e.to_string()))?;
    diffy::apply(input, &patch).map_err(|e| Error::Apply(e.to_string()))
}
