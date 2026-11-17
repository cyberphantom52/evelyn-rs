//! Packages (APKs and JARs) a `Package` edits, open in their target until the
//! build is written. Their code is edited as smali, a class at a time, and
//! assembled back into the DEX files in place. Only when an edit reaches the
//! package's files are they read into memory, by their paths in the package:
//! resources and the manifest as text when an edit reaches them, everything else
//! as it is, the DEX files left out. Writing it puts back only what the edits
//! changed, and signs it once.

use crate::recipe::{Expr, Location, Operation};
use apkscope_core::{apk::Apk, apksign, rebuild, resources, smali};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("cannot access {0}")]
    Io(PathBuf, #[source] std::io::Error),
    #[error("cannot edit the package")]
    Package(#[from] apkscope_core::Error),
}

/// The manifest, which with `res/` and the resource table is decoded as one.
const MANIFEST: &str = "AndroidManifest.xml";

/// A package open for the edits of every `Package` of it.
pub(crate) struct Package {
    /// The package as the build had it when it was opened.
    source: PathBuf,
    /// The key it is signed with when it is written, if a `Sign` says so.
    keys: Option<crate::toolchain::Keys>,
    /// Its files as the edits so far left them, if an edit reaches them.
    files: Option<Files>,
    /// The entries read into `files` as they are.
    read: BTreeSet<String>,
    /// Whether its resources and manifest are in `files` too, as text.
    resources: bool,
    /// The classes edited so far, by descriptor, as smali.
    classes: BTreeMap<String, String>,
}

/// A package's files, by their paths from its root with `/` between names. A
/// directory is there while a file is below it.
#[derive(Default)]
pub(crate) struct Files(BTreeMap<String, Vec<u8>>);

impl Files {
    /// The names directly in `dir` (`""` for the root), files and directories.
    pub(crate) fn children(&self, dir: &str) -> Vec<String> {
        let prefix = match dir {
            "" => String::new(),
            dir => format!("{dir}/"),
        };
        let mut names: Vec<String> = self
            .0
            .range(prefix.clone()..)
            .take_while(|(name, _)| name.starts_with(&prefix))
            .map(|(name, _)| {
                let rest = &name[prefix.len()..];
                rest.split('/').next().unwrap_or(rest).to_owned()
            })
            .collect();
        names.dedup();
        names
    }

    /// Whether `path` is a file, or a directory with a file below it.
    pub(crate) fn exists(&self, path: &str) -> bool {
        self.0.contains_key(path) || self.is_dir(path)
    }

    /// Whether `path` is a directory with a file below it.
    pub(crate) fn is_dir(&self, path: &str) -> bool {
        let prefix = format!("{path}/");
        self.0
            .range(prefix.clone()..)
            .next()
            .is_some_and(|(name, _)| name.starts_with(&prefix))
    }

    /// The content of the file at `path`.
    pub(crate) fn read(&self, path: &str) -> Option<&[u8]> {
        self.0.get(path).map(Vec::as_slice)
    }

    /// Put `data` at `path`, replacing a file there.
    pub(crate) fn write(&mut self, path: &str, data: Vec<u8>) {
        self.0.insert(path.to_owned(), data);
    }

    /// Remove the file at `path`, or everything below the directory there.
    pub(crate) fn remove(&mut self, path: &str) {
        let prefix = format!("{path}/");
        self.0
            .retain(|name, _| name != path && !name.starts_with(&prefix));
    }
}

impl Package {
    /// The package at `source`.
    pub(crate) fn new(source: PathBuf) -> Self {
        Self {
            source,
            keys: None,
            files: None,
            read: BTreeSet::new(),
            resources: false,
            classes: BTreeMap::new(),
        }
    }

    /// Sign the package with `keys` when it is written.
    pub(crate) fn sign(&mut self, keys: crate::toolchain::Keys) {
        self.keys = Some(keys);
    }

    /// Read what `edits` reach that is not read yet: the package's files, and its
    /// resources if they reach those.
    pub(crate) fn prepare(&mut self, edits: &[Operation]) -> Result<(), Error> {
        if edits.iter().all(code) {
            return Ok(());
        }
        let resources = resources(edits);
        match &mut self.files {
            None => {
                let (files, read) = read_files(&self.source, resources)?;
                (self.files, self.read, self.resources) = (Some(files), read, resources);
            }
            Some(files) if resources && !self.resources => {
                for (name, data) in apkscope_core::resources::decode(&mut open_apk(&self.source)?)?
                {
                    files.write(&name, data);
                }
                self.resources = true;
            }
            Some(_) => {}
        }
        Ok(())
    }
}

/// Read the files of the package at `source`: every entry but the DEX files,
/// the resources and the manifest as they are, and those as text if
/// `resources`. With them, the entries read as they are, by name.
fn read_files(source: &Path, resources: bool) -> Result<(Files, BTreeSet<String>), Error> {
    let mut apk = open_apk(source)?;
    let mut files = Files::default();
    let mut read = BTreeSet::new();
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
        files.write(&name, data);
        read.insert(name);
    }
    if resources {
        for (name, data) in resources::decode(&mut apk)? {
            files.write(&name, data);
        }
    }
    Ok((files, read))
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

impl Package {
    /// The package's files, which an edit outside its code reaches.
    pub(crate) fn files(&self) -> Result<&Files, crate::patch::Error> {
        self.files.as_ref().ok_or(crate::patch::Error::Unsupported(
            "a package's files are read only when an edit reaches them",
        ))
    }

    /// The package's files, to change.
    pub(crate) fn files_mut(&mut self) -> Result<&mut Files, crate::patch::Error> {
        self.files.as_mut().ok_or(crate::patch::Error::Unsupported(
            "a package's files are read only when an edit reaches them",
        ))
    }

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
    fn edits(&self, apk: &mut Apk) -> Result<rebuild::Edits, Error> {
        let mut edits = rebuild::Edits::new();
        if let Some(Files(files)) = &self.files {
            let mut files = files.clone();
            let decoded: BTreeMap<String, Vec<u8>> = match self.resources {
                true => {
                    let decoded = files.extract_if(.., |name, _| !raw(name)).collect();
                    edits.extend(resources::edits(apk, &decoded, true)?);
                    decoded
                }
                false => BTreeMap::new(),
            };
            for name in &self.read {
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
            let classes: Vec<String> = self.classes.values().cloned().collect();
            smali::inject_into(apk, &classes, &mut edits)?;
        }
        Ok(edits)
    }

    /// The package as its edits left it, with only what they changed changed,
    /// stored entries aligned, and signed if it has keys.
    pub(crate) fn render(&self) -> Result<Vec<u8>, Error> {
        let mut apk = open_apk(&self.source)?;
        let edits = self.edits(&mut apk)?;
        let file = fs::File::open(&self.source).map_err(io(&self.source))?;
        let (mut bytes, central_directory) = rebuild::rebuild_from(file, &edits)?;
        if let Some(keys) = &self.keys {
            let key = apksign::load(&keys.key, &keys.certificate)?;
            bytes = apksign::sign(bytes, central_directory, &key)?;
        }
        Ok(bytes)
    }
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
        Operation::Remove(path)
        | Operation::Text(path, _)
        | Operation::Bytes(path, _)
        | Operation::Xml(path, _)
        | Operation::AssertHash(path, _) => {
            resource(path)
                || edit.locations().iter().any(|location| {
                    !matches!(location, Location::Partition(_)) && resource(location.path())
                })
        }
        Operation::Resources(_) => true,
        Operation::Copy { from, to: dest, .. } => {
            resource(from.path()) || dest.as_ref().is_some_and(to)
        }
        Operation::Install(asset, dest) => resource(asset) || to(dest),
        Operation::Assert(condition, _) | Operation::Let(_, condition) => selects(condition),
        Operation::ForEach(over, _, operations) => selects(over) || resources(operations),
        Operation::When(condition, operations) => selects(condition) || resources(operations),
        _ => false,
    }
}

pub(crate) fn io(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    |source| Error::Io(path.into(), source)
}
