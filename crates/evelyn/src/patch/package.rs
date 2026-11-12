//! Packages (APKs and JARs) a `Package` edits. Their code is edited as smali, a
//! class at a time, and assembled back into the DEX files in place: nothing else
//! of the package is touched. Only when an edit reaches the package's files is
//! it decoded with apktool into a directory those edits run in. A failed edit
//! leaves that directory partially changed; the next decode replaces it.

use crate::patch::{Target, compose::Context};
use crate::recipe::{Expr, Location, Operation};
use android::partition::PartitionPath;
use android::path::RelativePath;
use apkscope_core::{apksign, rebuild, smali};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}: {1}")]
    Io(PathBuf, #[source] std::io::Error),
    #[error("package: {0}")]
    Package(String),
}

/// A package open for the edits inside a `Package`.
pub(crate) struct Opened {
    /// The package as the build has it.
    source: PathBuf,
    /// Where its files are decoded, if an edit reaches them.
    pub decoded: PathBuf,
    files: bool,
    /// The partition the package is in.
    pub partition: String,
    rebuilt: PathBuf,
    /// The classes edited so far, by descriptor, as smali.
    classes: BTreeMap<String, String>,
}

/// Open the package at `at`. Its files are decoded only when `edits` reach
/// them, and its resources only when they reach those.
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
    let files = !edits.iter().all(code);
    match files {
        true => crate::patch::apktool::decode(context.tools, &source, &decoded, resources(edits))?,
        false => {
            let _ = fs::remove_dir_all(&decoded);
        }
    }
    Ok(Opened {
        source,
        decoded,
        files,
        partition: at.partition.clone(),
        rebuilt: workspace.join("rebuilt"),
        classes: BTreeMap::new(),
    })
}

impl Opened {
    /// The smali of `class`, a descriptor, as the edits so far left it.
    pub(crate) fn class(&mut self, class: &str) -> Result<&mut String, Error> {
        if !self.classes.contains_key(class) {
            let text = smali::export_class(&self.source.to_string_lossy(), class)
                .map_err(Error::Package)?;
            self.classes.insert(class.to_owned(), text);
        }
        Ok(self.classes.get_mut(class).expect("inserted above"))
    }

    /// The classes whose smali has `text` in it, every DEX file's included.
    pub(crate) fn classes_with(&mut self, text: &str) -> Result<Vec<String>, Error> {
        let apk =
            apkscope_core::apk::open(&self.source.to_string_lossy()).map_err(Error::Package)?;
        for (_, dex) in &apk.dex {
            for class in smali::read::read(dex).map_err(Error::Package)? {
                let name = String::from_utf8_lossy(&class.name).into_owned();
                if self.classes.contains_key(&name) {
                    continue;
                }
                let smali = smali::text::print(&class);
                if smali.contains(text) {
                    self.classes.insert(name, smali);
                }
            }
        }
        let with = self
            .classes
            .iter()
            .filter(|(_, smali)| smali.contains(text));
        Ok(with.map(|(name, _)| name.clone()).collect())
    }
}

/// Put the package back at `path` in `target`: its files rebuilt if they were
/// decoded, the edited classes assembled into its DEX files, stored entries
/// aligned. An APK is signed with Evelyn's key; a JAR is not verified and stays
/// unsigned.
pub(crate) fn close(
    target: &mut Target,
    path: &RelativePath,
    opened: Opened,
    context: &Context<'_>,
) -> Result<(), crate::patch::Error> {
    let mut package = opened.source;
    if opened.files {
        package = opened.rebuilt.with_extension("files");
        crate::patch::apktool::build(context.tools, &opened.decoded, &package)?;
    }
    let package = package.to_string_lossy();
    let mut edits = rebuild::Edits::new();
    if !opened.classes.is_empty() {
        let classes: Vec<String> = opened.classes.into_values().collect();
        smali::inject(&package, &classes, &mut edits).map_err(Error::Package)?;
    }
    let (mut bytes, central_directory) =
        rebuild::rebuild(&package, &edits).map_err(Error::Package)?;
    if path.as_ref().ends_with(".apk") {
        let keys = context.tools.keys()?;
        let key = apksign::load(&keys.key, &keys.certificate).map_err(Error::Package)?;
        bytes = apksign::sign(bytes, central_directory, &key).map_err(Error::Package)?;
    }
    fs::write(&opened.rebuilt, bytes).map_err(io(&opened.rebuilt))?;
    target.replace_file(path, &opened.rebuilt)
}

/// Whether an edit changes code only, and so needs none of the package's files.
fn code(edit: &Operation) -> bool {
    matches!(
        edit,
        Operation::StubMethod { .. }
            | Operation::ReplaceMethod { .. }
            | Operation::InsertMethod { .. }
            | Operation::ReplaceOperand { .. }
    )
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
