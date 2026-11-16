//! Unified diffs applied to a file.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid unified diff")]
    InvalidPatch(#[source] diffy::ParsePatchError),
    #[error("diff does not apply")]
    Apply(#[source] diffy::ApplyError),
}

pub fn apply(input: &str, patch: &str) -> Result<String, Error> {
    let patch = diffy::Patch::from_str(patch).map_err(Error::InvalidPatch)?;
    diffy::apply(input, &patch).map_err(Error::Apply)
}
