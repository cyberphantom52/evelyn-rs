//! Packages (APKs and JARs) a `Package` edits. Their code is edited as smali, a
//! class at a time, and assembled back into the DEX files in place. Only when an
//! edit reaches the package's files are they written out to a directory those
//! edits run in, laid out as they are in the package: resources and the manifest
//! as text when an edit reaches them, everything else as it is, the DEX files
//! left out. Closing puts back only what the edits changed. A failed edit leaves
//! that directory partially changed; the next open replaces it.

use crate::patch::{Target, compose::Context};
use crate::recipe::{Expr, Location, Operation};
use android::partition::PartitionPath;
use android::path::RelativePath;
use apkscope_core::{apk::Apk, apksign, rebuild, resources, smali};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}: {1}")]
    Io(PathBuf, #[source] std::io::Error),
    #[error("package: {}", .0.report())]
    Package(#[from] apkscope_core::Error),
}

/// The manifest, which with `res/` and the resource table is decoded as one.
const MANIFEST: &str = "AndroidManifest.xml";

/// A package open for the edits inside a `Package`.
pub(crate) struct Opened {
    /// The package as the build has it.
    source: PathBuf,
    /// Where its files are written, if an edit reaches them.
    pub decoded: PathBuf,
    /// The entries written there as they are, if any were.
    files: Option<BTreeSet<String>>,
    /// Whether its resources and manifest were written there too, as text.
    resources: bool,
    /// The partition the package is in.
    pub partition: String,
    rebuilt: PathBuf,
    /// The classes edited so far, by descriptor, as smali.
    classes: BTreeMap<String, String>,
}

/// Open the package at `at`. Its files are written out only when `edits` reach
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
    let _ = fs::remove_dir_all(&decoded);
    let resources = resources(edits);
    let files = match edits.iter().all(code) {
        true => None,
        false => Some(write_out(&source, &decoded, resources)?),
    };
    Ok(Opened {
        source,
        decoded,
        files,
        resources,
        partition: at.partition.clone(),
        rebuilt: workspace.join("rebuilt"),
        classes: BTreeMap::new(),
    })
}

/// Write the files of the package at `source` to `to`: every entry but the DEX
/// files, the resources and the manifest as they are, and those as text if
/// `resources`. The entries written as they are, by name.
fn write_out(source: &Path, to: &Path, resources: bool) -> Result<BTreeSet<String>, Error> {
    let mut apk = open_apk(source)?;
    let mut written = BTreeSet::new();
    for i in 0..apk.zip.len() {
        let entry = apk.zip.by_index(i).map_err(apkscope_core::Error::from)?;
        let Some(name) = entry.enclosed_name().filter(|_| entry.is_file()) else {
            continue;
        };
        let name = name.to_string_lossy().replace('\\', "/");
        if !raw(&name) {
            continue;
        }
        drop(entry);
        let data = apk
            .read(&name)
            .ok_or_else(|| apkscope_core::Error::EntryNotFound(name.clone()))?;
        write(&to.join(&name), &data)?;
        written.insert(name);
    }
    if resources {
        for (name, data) in resources::decode(&mut apk)? {
            write(&to.join(name), &data)?;
        }
    }
    Ok(written)
}

/// Whether an entry is written out as it is: one that is neither code nor the
/// resources and manifest, which are decoded together.
fn raw(name: &str) -> bool {
    let dex = !name.contains('/') && name.starts_with("classes") && name.ends_with(".dex");
    !dex && name != MANIFEST && name != "resources.arsc" && !name.starts_with("res/")
}

fn open_apk(path: &Path) -> Result<Apk, Error> {
    Ok(Apk::new(fs::File::open(path).map_err(io(path))?)?)
}

fn write(path: &Path, data: &[u8]) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io(parent))?;
    }
    fs::write(path, data).map_err(io(path))
}

/// Every file under `dir`, by its path from `dir` with `/` between names.
fn files_under(dir: &Path) -> Result<BTreeMap<String, Vec<u8>>, Error> {
    let mut files = BTreeMap::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(d) = dirs.pop() {
        for entry in fs::read_dir(&d).map_err(io(&d))? {
            let path = entry.map_err(io(&d))?.path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                let name = path.strip_prefix(dir).expect("under dir");
                let name = name.to_string_lossy().replace('\\', "/");
                files.insert(name, fs::read(&path).map_err(io(&path))?);
            }
        }
    }
    Ok(files)
}

impl Opened {
    /// The smali of `class`, a descriptor, as the edits so far left it.
    pub(crate) fn class(&mut self, class: &str) -> Result<&mut String, Error> {
        if !self.classes.contains_key(class) {
            let text = smali::export_class_from(&mut open_apk(&self.source)?, class)?;
            self.classes.insert(class.to_owned(), text);
        }
        Ok(self.classes.get_mut(class).expect("inserted above"))
    }

    /// The classes whose smali has `text` in it, every DEX file's included.
    pub(crate) fn classes_with(&mut self, text: &str) -> Result<Vec<String>, Error> {
        let apk = open_apk(&self.source)?;
        for (_, dex) in &apk.dex {
            for class in smali::read::read(dex)? {
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

    /// How the package's entries change: the files the edits changed, added or
    /// removed, and the edited classes assembled into its DEX files.
    fn edits(self, apk: &mut Apk) -> Result<rebuild::Edits, Error> {
        let mut edits = rebuild::Edits::new();
        if let Some(written) = &self.files {
            let mut files = files_under(&self.decoded)?;
            let decoded: BTreeMap<String, Vec<u8>> = match self.resources {
                true => {
                    let decoded = files.extract_if(.., |name, _| !raw(name)).collect();
                    edits.extend(resources::edits(apk, &decoded, true)?);
                    decoded
                }
                false => BTreeMap::new(),
            };
            for name in written {
                if !files.contains_key(name) {
                    edits.insert(name.clone(), None);
                }
            }
            for (name, data) in files {
                if !raw(&name) && !decoded.contains_key(&name) {
                    // a resource file next to resources that were not decoded
                    return Err(apkscope_core::Error::EntryNotFound(format!(
                        "{name}: resources and the manifest are edited only through text"
                    ))
                    .into());
                }
                if apk.read(&name).as_ref() != Some(&data) {
                    edits.insert(name, Some(data));
                }
            }
        }
        if !self.classes.is_empty() {
            let classes: Vec<String> = self.classes.into_values().collect();
            smali::inject_into(apk, &classes, &mut edits)?;
        }
        Ok(edits)
    }
}

/// Put the package back at `path` in `target`, with only what its edits changed
/// changed and stored entries aligned, and signed with Evelyn's key if `sign`.
pub(crate) fn close(
    target: &mut Target,
    path: &RelativePath,
    opened: Opened,
    sign: bool,
    context: &Context<'_>,
) -> Result<(), crate::patch::Error> {
    let source = opened.source.clone();
    let rebuilt = opened.rebuilt.clone();
    let mut apk = open_apk(&source)?;
    let edits = opened.edits(&mut apk)?;
    let file = fs::File::open(&source).map_err(io(&source))?;
    let (mut bytes, central_directory) =
        rebuild::rebuild_from(file, &edits).map_err(Error::from)?;
    if sign {
        let keys = context.tools.keys()?;
        let key = apksign::load(&keys.key, &keys.certificate).map_err(Error::from)?;
        bytes = apksign::sign(bytes, central_directory, &key).map_err(Error::from)?;
    }
    fs::write(&rebuilt, bytes).map_err(io(&rebuilt))?;
    target.replace_file(path, &rebuilt)
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
