//! Turn an edited copy of a file into recipe operations.

use android_firmware::{DevicePath, RelativePath, Sha256Digest};
use recipe::{Hex, Operation};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureKind {
    /// A unified patch, applied to the file and its hardlinks.
    Text,
    /// Same-width byte edits, applied to the file and its hardlinks.
    Binary,
    /// A new file installed over this path alone, keeping its metadata.
    Replace,
}

#[derive(Debug)]
pub struct Captured {
    pub operations: Vec<Operation>,
    /// Unified patch bytes for Text, output bytes for Replace, absent for Binary.
    pub asset: Option<Vec<u8>>,
    pub after: Sha256Digest,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("capture requires changed bytes")]
    NoChange,
    #[error("text capture requires UTF-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("binary capture requires equal input and output lengths")]
    BinaryLength,
}

/// Operations turning `before` into `after`. They check the content they change
/// (the patch's context, the replaced bytes), not the whole file's hash, so they
/// keep applying when other parts of the file change between firmware builds.
pub fn capture(
    before: &[u8],
    after: &[u8],
    target: DevicePath,
    asset: RelativePath,
    kind: CaptureKind,
) -> Result<Captured, Error> {
    if before == after {
        return Err(Error::NoChange);
    }
    let digest = Sha256Digest::bytes(after);
    let (operations, asset) = match kind {
        CaptureKind::Text => {
            let before = std::str::from_utf8(before)?;
            let after = std::str::from_utf8(after)?;
            let patch = diffy::create_patch(before, after).to_string().into_bytes();
            (
                vec![Operation::Diff {
                    target,
                    patch: asset,
                }],
                Some(patch),
            )
        }
        CaptureKind::Binary => {
            if before.len() != after.len() {
                return Err(Error::BinaryLength);
            }
            let mut edits = BTreeMap::new();
            let mut cursor = 0;
            while cursor < before.len() {
                if before[cursor] == after[cursor] {
                    cursor += 1;
                    continue;
                }
                let start = cursor;
                while cursor < before.len() && before[cursor] != after[cursor] {
                    cursor += 1;
                }
                edits.insert(start as u64, Hex(after[start..cursor].to_vec()));
            }
            (
                vec![Operation::WriteBytes {
                    target,
                    before: Sha256Digest::bytes(before),
                    edits,
                }],
                None,
            )
        }
        CaptureKind::Replace => (
            vec![Operation::Copy {
                from: recipe::tree::Source::Assets,
                path: asset.to_string(),
                to: Some(target),
                metadata: recipe::Metadata::Copy,
            }],
            Some(after.to_vec()),
        ),
    };
    Ok(Captured {
        operations,
        asset,
        after: digest,
    })
}
