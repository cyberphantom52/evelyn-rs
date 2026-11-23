//! Packages (APKs and JARs) a `Package` edits, open in their target until the
//! build is written, as apkscope keeps an APK open: opening one reads the list
//! of its entries, an edit reads what it reaches, and only what the edits
//! changed is kept. Its files are at their paths in the package, resources and
//! the manifest as text, a file resource where its type, name and configuration
//! put it; its code is edited a class at a time, as apkscope models it; a value
//! resource is set in the resource table. Writing it puts back only what the
//! edits changed, and signs it once.

use crate::recipe::ResourceValue;
use apkscope_core::smali::{Document, model::Class};
use apkscope_core::{apk::Apk, apksign, values::Body};
use std::{
    cell::RefCell,
    collections::BTreeSet,
    fs,
    io::{self, Read, Seek, Write},
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("cannot access {0}")]
    Io(PathBuf, #[source] io::Error),
    #[error("cannot edit the package")]
    Package(#[from] apkscope_core::Error),
}

/// A package open for the edits of every `Package` of it.
pub(crate) struct Package {
    /// The package as the build had it when it was opened, with the edits so
    /// far. Reading from it moves in its file and keeps what was read, which
    /// changes nothing of what it is.
    apk: RefCell<Apk>,
    /// The key it is signed with when it is written, if a `Sign` says so.
    keys: Option<crate::toolchain::Keys>,
}

/// The paths of a package's files, from its root with `/` between names. A
/// directory is there while a file is below it.
pub(crate) struct Files(BTreeSet<String>);

impl Files {
    /// The names directly in `dir` (`""` for the root), files and directories.
    pub(crate) fn children(&self, dir: &str) -> Vec<String> {
        let prefix = match dir {
            "" => String::new(),
            dir => format!("{dir}/"),
        };
        let mut names: Vec<String> = self
            .below(&prefix)
            .map(|name| {
                let rest = &name[prefix.len()..];
                rest.split('/').next().unwrap_or(rest).to_owned()
            })
            .collect();
        names.dedup();
        names
    }

    /// The paths that start with `prefix`.
    fn below<'a>(&'a self, prefix: &'a str) -> impl Iterator<Item = &'a String> {
        self.0
            .range(prefix.to_owned()..)
            .take_while(move |name| name.starts_with(prefix))
    }

    /// Whether `path` is a file, or a directory with a file below it.
    pub(crate) fn exists(&self, path: &str) -> bool {
        self.0.contains(path) || self.is_dir(path)
    }

    /// Whether `path` is a directory with a file below it.
    pub(crate) fn is_dir(&self, path: &str) -> bool {
        self.below(&format!("{path}/")).next().is_some()
    }
}

impl Package {
    /// The package at `source`.
    pub(crate) fn open(source: &Path) -> Result<Self, Error> {
        let file = fs::File::open(source).map_err(io(source))?;
        Ok(Self {
            apk: RefCell::new(Apk::new(file)?),
            keys: None,
        })
    }

    /// Sign the package with `keys` when it is written.
    pub(crate) fn sign(&mut self, keys: crate::toolchain::Keys) {
        self.keys = Some(keys);
    }

    /// The paths of the package's files, as the edits so far left them.
    pub(crate) fn files(&self) -> Result<Files, Error> {
        Ok(Files(self.apk.borrow_mut().files()?.into_iter().collect()))
    }

    /// The content of the file at `path`, as the edits so far left it.
    pub(crate) fn read(&self, path: &str) -> Result<Option<Vec<u8>>, Error> {
        Ok(self.apk.borrow_mut().file(path)?)
    }

    /// Put `data` at `path`, replacing a file there.
    pub(crate) fn write(&mut self, path: &str, data: Vec<u8>) -> Result<(), Error> {
        Ok(self.apk.get_mut().put(path, data)?)
    }

    /// Remove the file at `path`, or everything below the directory there.
    pub(crate) fn remove(&mut self, path: &str) -> Result<(), Error> {
        let files = self.files()?;
        let below = format!("{path}/");
        let apk = self.apk.get_mut();
        for file in files.0.get(path).into_iter().chain(files.below(&below)) {
            apk.remove(file)?;
        }
        Ok(())
    }

    /// Set the value resource `name` of the kind `kind`, as a values file's
    /// element names kinds, in the configuration `config` names. Whether it had
    /// a value there.
    pub(crate) fn set_resource(
        &mut self,
        kind: &str,
        name: &str,
        config: &str,
        value: &ResourceValue,
    ) -> Result<bool, Error> {
        let body = match value {
            ResourceValue::One(text) => Body::Text(text),
            ResourceValue::Items(items) => Body::Items(items),
        };
        Ok(self.apk.get_mut().set_value(kind, name, config, body)?)
    }

    /// The class `class`, a descriptor, as the edits so far left it, with the
    /// DEX file it is of: what the class refers to is in that file's source, and
    /// setting the class in the file is an edit, unless it is the class as the
    /// build had it.
    pub(crate) fn class(&mut self, class: &str) -> Result<(&mut Document, Class), Error> {
        Ok(self.apk.get_mut().classes()?.class(class)?)
    }

    /// The classes, as the edits so far left them, with an instruction that has
    /// `operand`, written as smali.
    pub(crate) fn classes_with(&mut self, operand: &str) -> Result<Vec<String>, Error> {
        Ok(self.apk.get_mut().classes()?.find(operand)?)
    }

    /// Write the package as its edits left it into `out`, with only what they
    /// changed changed and stored entries aligned, then sign it there if it has
    /// keys. How many bytes that is.
    pub(crate) fn render<F: Read + Write + Seek>(&self, out: &mut F) -> Result<u64, Error> {
        let size = self
            .apk
            .borrow_mut()
            .rebuild(io::BufWriter::new(&mut *out))?;
        match &self.keys {
            Some(keys) => {
                let key = apksign::load(&keys.key, &keys.certificate)?;
                Ok(apksign::sign(out, &key)?)
            }
            None => Ok(size),
        }
    }
}

pub(crate) fn io(path: &Path) -> impl FnOnce(io::Error) -> Error + '_ {
    |source| Error::Io(path.into(), source)
}
