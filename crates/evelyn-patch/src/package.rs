//! Packages (APKs and JARs) a `Package` edits: decoded with apktool into a
//! directory its body runs in, then rebuilt in place of the original. A failed
//! body leaves the directory partially changed; the next decode replaces it.

use crate::{Target, compose::Context};
use android_firmware::{Name, RelativePath, TargetPath};
use recipe::{
    tree::{Expr, Stmt},
    visit::{self, Visitor},
};
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

/// A decoded package, open for the statements inside a `Package`.
pub(crate) struct Opened {
    pub decoded: PathBuf,
    /// The partition the package is in.
    pub partition: Name,
    rebuilt: PathBuf,
}

/// Decode the package at `at`. Resources are decoded only when `body` touches them.
pub(crate) fn open(
    target: &mut Target,
    at: &TargetPath,
    body: &[Stmt],
    context: &Context<'_>,
    step: &Name,
) -> Result<Opened, crate::Error> {
    let source = target.content_path(&at.path, context.images)?;
    let workspace = context.packages.join(step.as_ref());
    fs::create_dir_all(&workspace)?;
    let decoded = workspace.join("decoded");
    let mut resources = Resources(false);
    visit::walk(body, &mut resources)?;
    crate::apktool::decode(context.tools, &source, &decoded, resources.0)?;
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
) -> Result<(), crate::Error> {
    let sign = path.as_ref().ends_with(".apk");
    crate::apktool::build(context.tools, &opened.decoded, &opened.rebuilt, sign)?;
    target.replace_file(path, &opened.rebuilt)
}

/// Whether statements reach resources or the manifest, which need decoding.
struct Resources(bool);

impl Resources {
    /// A path that is computed may be anything.
    fn note_expr(&mut self, path: &Expr) {
        self.0 |= path
            .literal_str()
            .is_none_or(|path| path.starts_with("/res/") || path == "/AndroidManifest.xml");
    }
}

impl Visitor for Resources {
    fn stmt(
        &mut self,
        stmt: &Stmt,
        _bound: &[&recipe::tree::Binding],
    ) -> Result<(), recipe::Error> {
        if let Stmt::Copy { to: Some(to), .. } = stmt {
            self.note_expr(to);
        }
        Ok(())
    }

    fn expr(
        &mut self,
        expr: &Expr,
        _bound: &[&recipe::tree::Binding],
    ) -> Result<(), recipe::Error> {
        if let Expr::Select { pattern, .. } = expr {
            self.note_expr(pattern);
        }
        Ok(())
    }
}

pub(crate) fn io(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    |source| Error::Io(path.into(), source)
}
