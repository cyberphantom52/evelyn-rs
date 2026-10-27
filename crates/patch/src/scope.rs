//! What a recipe reads from the composed build and the firmware: selections,
//! property values and files, and the value of every expression.
//!
//! A property's device-wide value is the one from the highest-precedence partition
//! that defines it. Precedence is the old Evelyn shell's order: partitions outside
//! the standard five first (`my_*`, `*_dlkm`), by name, then `product`, `odm`,
//! `vendor`, `system_ext`, `system`, reordered by `ro.product.property_source_order`
//! when the firmware sets it.

use crate::{Composition, Error, Target, compose::Context, layout, package::Opened};
use model::{DevicePath, EntryKind, Name, PartitionTree, RelativePath, SourceRef};
use recipe::{
    Firmware, PropertyTarget,
    tree::{BinaryOp, Expr, Scope as Range, UnaryOp},
    value::{Located, Place, Value},
};
use std::collections::BTreeMap;

const STANDARD: [&str; 5] = ["product", "odm", "vendor", "system_ext", "system"];

/// Where a partition's own properties go when it defines none of a key.
const MAIN_FILES: [&str; 3] = ["system/build.prop", "etc/build.prop", "build.prop"];

/// The files in one partition that define a key, and the value they agree on.
pub(crate) struct Found {
    pub partition: Name,
    pub value: String,
    pub files: Vec<RelativePath>,
}

pub(crate) struct Scope<'a> {
    pub targets: &'a BTreeMap<Name, Target>,
    /// Which firmware each target partition was built from.
    pub origins: &'a BTreeMap<Name, Firmware>,
    pub context: &'a Context<'a>,
    /// The step being applied, for errors.
    pub step: &'a Name,
    /// The package a `Within` has open, where `/` is the package's root.
    pub package: Option<&'a Opened>,
}

impl Scope<'_> {
    /// The value of `key`: device-wide without a partition, from every `.prop` file
    /// in the partition without a path, else from that file.
    pub fn property(
        &self,
        partition: Option<&Name>,
        path: Option<&RelativePath>,
        key: &str,
    ) -> Result<Option<Found>, Error> {
        match partition {
            Some(partition) => self.in_partition(partition, path, key),
            None => {
                for partition in self.precedence()? {
                    if let Some(found) = self.in_partition(&partition, None, key)? {
                        return Ok(Some(found));
                    }
                }
                Ok(None)
            }
        }
    }

    fn in_partition(
        &self,
        partition: &Name,
        path: Option<&RelativePath>,
        key: &str,
    ) -> Result<Option<Found>, Error> {
        let Some(target) = self.targets.get(partition) else {
            return Ok(None);
        };
        let files = match path {
            Some(path) if target.tree.entries.contains_key(path) => vec![path.clone()],
            Some(_) => Vec::new(),
            None => property_files(target),
        };
        let mut found: Option<Found> = None;
        for file in files {
            let bytes = target.read(&file, self.context.images)?;
            let Some(value) = std::str::from_utf8(&bytes)
                .ok()
                .and_then(|text| android_property::get(text, key))
            else {
                continue;
            };
            match &mut found {
                Some(found) if found.value != value => {
                    return Err(Error::PropertyConflict {
                        partition: partition.clone(),
                        key: key.into(),
                    });
                }
                Some(found) => found.files.push(file),
                None => {
                    found = Some(Found {
                        partition: partition.clone(),
                        value: value.into(),
                        files: vec![file],
                    })
                }
            }
        }
        Ok(found)
    }

    /// Partitions from the highest precedence (loaded last) to the lowest.
    pub fn precedence(&self) -> Result<Vec<Name>, Error> {
        let present = |name: &str| self.targets.keys().find(|key| key.as_ref() == name);
        let extras: Vec<&Name> = self
            .targets
            .keys()
            .filter(|name| !STANDARD.contains(&name.as_ref()))
            .collect();
        let mut standard: Vec<&Name> = STANDARD.iter().filter_map(|name| present(name)).collect();
        let mut first_extras = Vec::new();
        let bootstrap = extras.iter().chain(&standard).copied().collect::<Vec<_>>();
        let mut order = None;
        for partition in bootstrap {
            if let Some(found) =
                self.in_partition(partition, None, "ro.product.property_source_order")?
            {
                order = Some(found.value);
                break;
            }
        }
        if let Some(order) = order {
            let tokens: Vec<&str> = order
                .split(',')
                .map(str::trim)
                .filter(|token| !token.is_empty())
                .collect();
            let valid = tokens.iter().all(|token| {
                STANDARD.contains(token) || extras.iter().any(|e| e.as_ref() == *token)
            });
            if valid {
                let listed_standard: Vec<&Name> = tokens
                    .iter()
                    .filter(|token| STANDARD.contains(*token))
                    .filter_map(|token| present(token))
                    .collect();
                first_extras = tokens
                    .iter()
                    .filter_map(|token| extras.iter().copied().find(|e| e.as_ref() == *token))
                    .collect();
                standard.retain(|name| !listed_standard.contains(name));
                standard = listed_standard.into_iter().chain(standard).collect();
            }
        }
        let mut ordered: Vec<Name> = first_extras.iter().map(|name| (*name).clone()).collect();
        ordered.extend(
            extras
                .into_iter()
                .filter(|name| !first_extras.contains(name))
                .cloned(),
        );
        ordered.extend(standard.into_iter().cloned());
        Ok(ordered)
    }

    /// The file that gets `partition`'s new properties.
    pub fn main_file(&self, partition: &Name) -> Option<RelativePath> {
        let target = self.targets.get(partition)?;
        MAIN_FILES
            .iter()
            .map(|path| RelativePath::try_from(path.to_string()).expect("valid path"))
            .find(|path| target.tree.entries.contains_key(path))
    }

    /// The value of `key` in a property target's scope; a file that is in no
    /// partition of this build defines nothing.
    pub fn get(&self, target: Option<&PropertyTarget>, key: &str) -> Result<Option<String>, Error> {
        let found = match target {
            None => self.property(None, None, key)?,
            Some(PropertyTarget::Partition(partition)) => {
                self.property(Some(partition), None, key)?
            }
            Some(PropertyTarget::File(path)) => {
                let resolved = Composition::resolve(self.targets, self.context.mounts, path);
                match resolved.as_slice() {
                    [file] => self.property(Some(&file.partition), Some(&file.path), key)?,
                    _ => None,
                }
            }
        };
        Ok(found.map(|found| found.value))
    }

    /// What `pattern` selects. Inside a firmware, the pattern is a device path in
    /// it. In the build, a device path, or a relative pattern tried in the
    /// directory of every partition the range selects.
    pub fn select(&self, pattern: &str, range: &Range) -> Result<Vec<Located>, Error> {
        if let Some(firmware) = range.inside {
            let mut found = Vec::new();
            for at in self
                .context
                .mounts
                .resolve(&DevicePath::try_from(pattern.to_owned())?)
            {
                let source = source_of(firmware, &at.partition)?;
                let tree = self
                    .context
                    .sources
                    .get(&source)
                    .ok_or_else(|| Error::UnknownSource(source.clone()))?;
                for path in matches(tree, &at.path) {
                    found.push(Located {
                        place: Place::Firmware(firmware),
                        partition: at.partition.clone(),
                        path,
                    });
                }
            }
            return Ok(found);
        }
        if let Some(package) = self.package {
            let path = pattern.strip_prefix('/').ok_or(Error::Unsupported(
                "paths inside a package start at its root, /",
            ))?;
            let path = RelativePath::try_from(path.to_owned())?;
            if layout::is_glob(&path) {
                return Err(Error::Unsupported("globs inside a package"));
            }
            return Ok(match path.resolve(&package.decoded).exists() {
                true => vec![Located {
                    place: Place::Package,
                    partition: package.partition.clone(),
                    path,
                }],
                false => Vec::new(),
            });
        }
        let paths = if pattern.starts_with('/') {
            Composition::resolve(
                self.targets,
                self.context.mounts,
                &DevicePath::try_from(pattern.to_owned())?,
            )
        } else {
            let selected: Vec<&Name> = self
                .targets
                .keys()
                .filter(|name| {
                    range
                        .origin
                        .is_none_or(|origin| self.origins.get(*name) == Some(&origin))
                })
                .filter(|name| {
                    range
                        .partition
                        .as_ref()
                        .is_none_or(|glob| layout::wildcard(glob, name.as_ref()))
                })
                .collect();
            if selected.is_empty() {
                return Err(Error::EmptyScope {
                    step: self.step.clone(),
                });
            }
            let mut paths = Vec::new();
            for name in selected {
                let Some(directory) = self.context.mounts.directory(name) else {
                    continue;
                };
                let device = DevicePath::try_from(format!("{directory}/{pattern}"))?;
                paths.extend(
                    Composition::resolve(self.targets, self.context.mounts, &device)
                        .into_iter()
                        .filter(|at| &at.partition == name),
                );
            }
            paths
        };
        let mut found = Vec::new();
        for at in paths {
            if let Some(target) = self.targets.get(&at.partition) {
                for path in matches(&target.tree, &at.path) {
                    found.push(Located {
                        place: Place::Build,
                        partition: at.partition.clone(),
                        path,
                    });
                }
            }
        }
        Ok(found)
    }

    /// The property files an edit of `key` lands in. With a file, that file; with
    /// a partition, every file in it defining the key; device-wide, where the
    /// device's value comes from, or with `every` every definition. Where nothing
    /// defines it, with `create` the file a new key goes to (the partition's own,
    /// device-wide product's), else an error.
    pub fn prop_files(
        &self,
        key: &str,
        target: Option<&PropertyTarget>,
        create: bool,
        every: bool,
    ) -> Result<Vec<Located>, Error> {
        let located = |partition: Name, path: RelativePath| Located {
            place: Place::Build,
            partition,
            path,
        };
        if let Some(PropertyTarget::File(path)) = target {
            let mut found = Composition::resolve(self.targets, self.context.mounts, path);
            if found.len() != 1 {
                return Err(Error::AbsentPath {
                    step: self.step.clone(),
                    path: path.clone(),
                });
            }
            let found = found.remove(0);
            return Ok(vec![located(found.partition, found.path)]);
        }
        let partition = target.and_then(PropertyTarget::partition);
        if let Some(partition) = partition
            && !self.targets.contains_key(partition)
        {
            return Err(Error::AbsentPartition {
                step: self.step.clone(),
                partition: partition.clone(),
            });
        }
        let found = if partition.is_none() && every {
            let mut found = Vec::new();
            for partition in self.precedence()? {
                found.extend(self.property(Some(&partition), None, key)?);
            }
            found
        } else {
            self.property(partition, None, key)?.into_iter().collect()
        };
        if !found.is_empty() {
            return Ok(found
                .into_iter()
                .flat_map(|found| {
                    let partition = found.partition;
                    found
                        .files
                        .into_iter()
                        .map(move |file| located(partition.clone(), file))
                })
                .collect());
        }
        if !create {
            return Err(android_property::Error::Undefined(key.into()).into());
        }
        let partition = match partition {
            Some(partition) => partition.clone(),
            // Product's properties load last of the standard partitions, so a new
            // key there takes effect.
            None => match self.targets.keys().find(|name| name.as_ref() == "product") {
                Some(product) => product.clone(),
                None => self
                    .precedence()?
                    .into_iter()
                    .next()
                    .ok_or(Error::Unsupported("no partition to hold a new property"))?,
            },
        };
        let file = self
            .main_file(&partition)
            .ok_or_else(|| Error::NoPropertyFile(partition.clone()))?;
        Ok(vec![located(partition, file)])
    }

    /// The one rule for each kind of expression.
    pub fn eval(&self, expr: &Expr) -> Result<Value, Error> {
        Ok(match expr {
            Expr::Literal(value) => value.clone(),
            Expr::Select { pattern, scope } => Value::Selection(self.select(pattern, scope)?),
            Expr::Prop { key, target } => self
                .get(target.as_ref(), key)?
                .map_or(Value::Unset, Value::Str),
            Expr::PropFiles {
                key,
                target,
                create,
                every,
            } => Value::Selection(self.prop_files(key, target.as_ref(), *create, *every)?),
            Expr::Class(class) => {
                let package = self
                    .package
                    .ok_or(Error::Unsupported("Class selects inside a package"))?;
                let found = crate::package::locate(&package.decoded, class)?;
                Value::Selection(
                    found
                        .into_iter()
                        .map(|path| Located {
                            place: Place::Package,
                            partition: package.partition.clone(),
                            path,
                        })
                        .collect(),
                )
            }
            Expr::Hash(inner) => {
                let selection = self.eval(inner)?;
                let [entry] = selection.as_selection()? else {
                    return Err(Error::Unsupported("Hash needs exactly one file"));
                };
                if let (Place::Package, Some(package)) = (entry.place, self.package) {
                    let bytes = std::fs::read(entry.path.resolve(&package.decoded))?;
                    return Ok(Value::Str(model::Sha256Digest::bytes(&bytes).to_string()));
                }
                let target = self
                    .targets
                    .get(&entry.partition)
                    .ok_or_else(|| Error::UnknownPartition(entry.partition.clone()))?;
                Value::Str(target.file(&entry.path)?.2.to_string())
            }
            Expr::Content(inner) => {
                let selection = self.eval(inner)?;
                let [entry] = selection.as_selection()? else {
                    return Err(Error::Unsupported("Content needs exactly one file"));
                };
                Value::Str(
                    String::from_utf8(self.content(entry)?)
                        .map_err(|_| Error::Unsupported("Content reads UTF-8 text"))?,
                )
            }
            Expr::Containing { selection, text } => {
                let selection = self.eval(selection)?;
                let text = self.eval(text)?;
                let text = text
                    .as_str()?
                    .ok_or(Error::Unsupported("Containing needs a string that is set"))?;
                let mut found = Vec::new();
                for entry in selection.as_selection()? {
                    let content = self.content(entry)?;
                    if content
                        .windows(text.len())
                        .any(|window| window == text.as_bytes())
                    {
                        found.push(entry.clone());
                    }
                }
                Value::Selection(found)
            }
            Expr::Partition(name) => Value::Bool(self.targets.contains_key(name)),
            Expr::Asset(path) => Value::Bool(path.resolve(self.context.assets).exists()),
            Expr::Unary { op, expr } => {
                let value = self.eval(expr)?;
                match op {
                    UnaryOp::Not => (!value)?,
                    UnaryOp::Defined => value.defined()?,
                }
            }
            Expr::Binary { op, left, right } => {
                let left = self.eval(left)?;
                match op {
                    // `All` and `Any` stop at the first value that decides them.
                    BinaryOp::And if !left.as_bool()? => Value::Bool(false),
                    BinaryOp::Or if matches!(left, Value::Bool(true)) => Value::Bool(true),
                    BinaryOp::And => (left & self.eval(right)?)?,
                    BinaryOp::Or => (left | self.eval(right)?)?,
                    BinaryOp::Eq => left.equals(&self.eval(right)?)?,
                    BinaryOp::In => left.is_in(&self.eval(right)?)?,
                }
            }
        })
    }
}

impl Scope<'_> {
    /// The content of a selected file: in the build, a firmware or the open package.
    fn content(&self, entry: &Located) -> Result<Vec<u8>, Error> {
        match entry.place {
            Place::Build => self
                .targets
                .get(&entry.partition)
                .ok_or_else(|| Error::UnknownPartition(entry.partition.clone()))?
                .read(&entry.path, self.context.images),
            Place::Package => {
                let package = self
                    .package
                    .ok_or(Error::Unsupported("no package is open"))?;
                Ok(std::fs::read(entry.path.resolve(&package.decoded))?)
            }
            Place::Firmware(firmware) => {
                let source = source_of(firmware, &entry.partition)?;
                let tree = self
                    .context
                    .sources
                    .get(&source)
                    .ok_or_else(|| Error::UnknownSource(source.clone()))?;
                let mut found = tree
                    .entries
                    .get(&entry.path)
                    .ok_or_else(|| Error::Missing(entry.path.clone()))?;
                if let EntryKind::Hardlink { target } = &found.kind {
                    found = tree
                        .entries
                        .get(target)
                        .ok_or_else(|| Error::Missing(target.clone()))?;
                }
                let EntryKind::File { content, size, .. } = &found.kind else {
                    return Err(Error::Unsupported("only a regular file has content"));
                };
                let store = android_fs::Store {
                    images: self.context.images,
                    root: std::path::Path::new("/"),
                };
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut store.open(content, *size)?, &mut bytes)?;
                Ok(bytes)
            }
        }
    }
}

/// A build source, from a firmware and a partition.
pub(crate) fn source_of(from: Firmware, partition: &Name) -> Result<SourceRef, Error> {
    Ok(SourceRef {
        source: Name::try_from(from.name().to_owned())?,
        partition: partition.clone(),
    })
}

/// The entries `path` names in a tree: every match of a glob, else the entry if
/// it is there.
fn matches(tree: &PartitionTree, path: &RelativePath) -> Vec<RelativePath> {
    if layout::is_glob(path) {
        layout::expand(tree, path)
    } else if tree.entries.contains_key(path) {
        vec![path.clone()]
    } else {
        Vec::new()
    }
}

/// A target's regular `.prop` files (or hardlinks to them), in path order.
fn property_files(target: &Target) -> Vec<RelativePath> {
    target
        .tree
        .entries
        .iter()
        .filter(|(path, entry)| {
            path.as_ref().ends_with(".prop")
                && matches!(
                    entry.kind,
                    EntryKind::File { .. } | EntryKind::Hardlink { .. }
                )
        })
        .map(|(path, _)| path.clone())
        .collect()
}
