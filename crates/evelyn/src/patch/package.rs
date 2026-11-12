//! Packages (APKs and JARs) a `Package` edits: decoded with apktool into a
//! directory its edits run in, then rebuilt in place of the original. A failed
//! edit leaves the directory partially changed; the next decode replaces it.

use crate::patch::{Target, compose::Context};
use crate::recipe::{Expr, Location, Operation};
use android::partition::PartitionPath;
use android::path::RelativePath;
use std::{
    fs,
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}: {1}")]
    Io(PathBuf, #[source] std::io::Error),
}

/// A decoded package, open for the edits inside a `Package`.
pub(crate) struct Opened {
    pub decoded: PathBuf,
    /// The partition the package is in.
    pub partition: String,
    rebuilt: PathBuf,
}

/// Decode the package at `at`. Resources are decoded only when `edits` touch them.
pub(crate) fn open(
    target: &mut Target,
    at: &PartitionPath,
    edits: &[Operation],
    context: &Context<'_>,
) -> Result<Opened, crate::patch::Error> {
    let source = target.content_path(&at.path, context.images)?;
    let workspace = context.packages.join(&at.partition).join(at.path.as_ref());
    fs::create_dir_all(&workspace)?;
    let decoded = workspace.join("decoded");
    crate::patch::apktool::decode(context.tools, &source, &decoded, resources(edits))?;
    Ok(Opened {
        decoded,
        partition: at.partition.clone(),
        rebuilt: workspace.join("rebuilt"),
    })
}

/// Rebuild the package and replace it at `path` in `target`. APKs are signed.
pub(crate) fn close(
    target: &mut Target,
    path: &RelativePath,
    opened: Opened,
    context: &Context<'_>,
) -> Result<(), crate::patch::Error> {
    let sign = path.as_ref().ends_with(".apk");
    crate::patch::apktool::build(context.tools, &opened.decoded, &opened.rebuilt, sign)?;
    target.replace_file(path, &opened.rebuilt)
}

/// Whether edits reach resources or the manifest, which need decoding: whether a
/// path they name is in `/res/` or is `/AndroidManifest.xml`. A path naming a
/// bound value, or computed, may be anything.
fn resources(edits: &[Operation]) -> bool {
    edits.iter().any(resource)
}

/// Whether an edit reaches resources or the manifest.
fn resource(edit: &Operation) -> bool {
    let resource = |path: &str| {
        crate::recipe::written_out(path)
            .is_none_or(|path| path.starts_with("/res/") || path == "/AndroidManifest.xml")
    };
    let selects = |expr: &Expr| {
        expr.locations().into_iter().any(|location| {
            !matches!(location, Location::Partition(_)) && resource(location.path())
        })
    };
    let to = |to: &Expr| match to {
        Expr::Text(path) => resource(path),
        _ => true,
    };
    match edit {
        Operation::Remove { pattern: path, .. }
        | Operation::ApplyDiff { target: path, .. }
        | Operation::WriteBytes { target: path, .. }
        | Operation::EditXml { target: path, .. }
        | Operation::AssertHash { target: path, .. } => resource(path),
        Operation::Append { target, content } => resource(target) || selects(content),
        Operation::Copy { from, to: dest, .. } => {
            resource(from.path()) || dest.as_ref().is_some_and(to)
        }
        Operation::Install { asset, to: dest } => resource(asset) || to(dest),
        Operation::Assert { condition, .. } => selects(condition),
        Operation::ForEach {
            over, operations, ..
        } => selects(over) || resources(operations),
        Operation::When {
            condition,
            operations,
        } => selects(condition) || resources(operations),
        _ => false,
    }
}

pub(crate) fn io(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    |source| Error::Io(path.into(), source)
}
