//! Packages (APKs and JARs) a `Within` edits: decoded with apktool into a
//! directory its body runs in, then rebuilt in place of the original. A failed
//! body leaves the directory partially changed; the next decode replaces it.

use crate::{Target, compose::Context};
use model::{Name, RelativePath, Sha256Digest, TargetPath};
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
    #[error("class {class} is defined in {count} smali files; expected one")]
    Class { class: String, count: usize },
    #[error("{0}: {1}")]
    Io(PathBuf, #[source] std::io::Error),
}

/// A decoded package, open for the statements inside a `Within`.
pub(crate) struct Opened {
    pub decoded: PathBuf,
    /// The partition the package is in.
    pub partition: Name,
    rebuilt: PathBuf,
}

/// Decode the package at `at`, whose content must be `before`. Resources are
/// decoded only when `body` touches them.
pub(crate) fn open(
    target: &mut Target,
    at: &TargetPath,
    before: Option<&Sha256Digest>,
    body: &[Stmt],
    context: &Context<'_>,
    step: &Name,
) -> Result<Opened, crate::Error> {
    let source = target.content_path(&at.path, before, context.images)?;
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
    step: &Name,
) -> Result<(), crate::Error> {
    let sign = path.as_ref().ends_with(".apk");
    crate::apktool::build(context.tools, &opened.decoded, &opened.rebuilt, sign)?;
    target.replace_file(path, &opened.rebuilt, step)
}

/// Whether statements reach resources or the manifest, which need decoding.
struct Resources(bool);

impl Resources {
    fn note(&mut self, path: &str) {
        self.0 |= path.starts_with("/res/") || path == "/AndroidManifest.xml";
    }
}

impl Visitor for Resources {
    fn stmt(&mut self, stmt: &Stmt, _bound: &[&Name]) -> Result<(), recipe::Error> {
        if let Stmt::Copy { to: Some(to), .. } = stmt {
            self.note(to.as_ref());
        }
        Ok(())
    }

    fn expr(&mut self, expr: &Expr, _bound: &[&Name]) -> Result<(), recipe::Error> {
        if let Expr::Select { pattern, .. } = expr {
            self.note(pattern);
        }
        Ok(())
    }
}

/// Where `Lcom/example/Name;` is defined: `smali*/com/example/Name.smali`, in at
/// most one dex.
pub(crate) fn locate(decoded: &Path, class: &str) -> Result<Option<RelativePath>, Error> {
    let none = || Error::Class {
        class: class.into(),
        count: 0,
    };
    let name = class
        .strip_prefix('L')
        .and_then(|name| name.strip_suffix(';'))
        .ok_or_else(none)?;
    let mut found = Vec::new();
    for entry in fs::read_dir(decoded).map_err(io(decoded))?.flatten() {
        let dex = entry.file_name().to_string_lossy().into_owned();
        if dex.starts_with("smali") {
            let path = RelativePath::try_from(format!("{dex}/{name}.smali")).map_err(|_| none())?;
            if path.resolve(decoded).is_file() {
                found.push(path);
            }
        }
    }
    match found.len() {
        0 | 1 => Ok(found.pop()),
        count => Err(Error::Class {
            class: class.into(),
            count,
        }),
    }
}

pub(crate) fn io(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    |source| Error::Io(path.into(), source)
}
