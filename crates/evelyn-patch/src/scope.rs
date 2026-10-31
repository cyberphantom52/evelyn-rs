//! What a recipe reads from the composed build and the firmware: selections,
//! property values and files, and the value of every expression.
//!
//! A property's device-wide value is the one from the highest-precedence partition
//! that defines it. Precedence is the old Evelyn shell's order: partitions outside
//! the standard five first (`my_*`, `*_dlkm`), by name, then `product`, `odm`,
//! `vendor`, `system_ext`, `system`, reordered by `ro.product.property_source_order`
//! when the firmware sets it.

use crate::{Composition, Error, Target, compose::Context, layout, package::Opened};
use android_firmware::{DevicePath, Firmware, Name, RelativePath, SourceRef};
use android_fs::EntryKind;
use recipe::{
    PropertyTarget,
    tree::{Expr, Scope as Range, Source},
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
    /// The package a `Package` has open, where `/` is the package's root.
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
        if range.inside == Some(Source::Assets) {
            // An asset (file or directory) by its path in the recipe's directory.
            let path = RelativePath::try_from(pattern.to_owned())?;
            return Ok(match path.resolve(self.context.assets).exists() {
                true => vec![Located {
                    place: Place::Asset,
                    partition: Name::try_from("assets".to_owned())?,
                    path,
                }],
                false => Vec::new(),
            });
        }
        if let Some(firmware) = range.inside.and_then(Source::firmware) {
            let mut found = Vec::new();
            for at in self
                .context
                .mounts
                .resolve(&DevicePath::try_from(pattern.to_owned())?)
            {
                let source = source_of(firmware, &at.partition);
                let tree = self
                    .context
                    .sources
                    .get(&source)
                    .ok_or_else(|| Error::UnknownSource(source.clone()))?;
                for path in layout::matches(tree, &at.path) {
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
            // Segment by segment, reading only the directories a glob needs.
            let mut found = vec![String::new()];
            for segment in path.split('/') {
                let mut next = Vec::new();
                for dir in found {
                    let join = |name: &str| match dir.is_empty() {
                        true => name.to_owned(),
                        false => format!("{dir}/{name}"),
                    };
                    if segment.contains(['*', '?']) {
                        let Ok(entries) = std::fs::read_dir(package.decoded.join(&dir)) else {
                            continue;
                        };
                        for entry in entries.flatten() {
                            let name = entry.file_name().to_string_lossy().into_owned();
                            if android_firmware::glob(segment, &name) {
                                next.push(join(&name));
                            }
                        }
                    } else if package.decoded.join(join(segment)).exists() {
                        next.push(join(segment));
                    }
                }
                found = next;
            }
            found.sort();
            return found
                .into_iter()
                .map(|path| {
                    Ok(Located {
                        place: Place::Package,
                        partition: package.partition.clone(),
                        path: RelativePath::try_from(path)?,
                    })
                })
                .collect();
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
                        .is_none_or(|glob| android_firmware::glob(glob, name.as_ref()))
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
                for path in layout::matches(&target.tree, &at.path) {
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

    /// The property files that define `key`: with a file, it if it does; with a
    /// partition, every file in it that does; without, every file in every
    /// partition that does.
    pub fn property_files(
        &self,
        key: &str,
        target: Option<&PropertyTarget>,
    ) -> Result<Vec<Located>, Error> {
        let found = match target {
            Some(PropertyTarget::File(_)) => {
                let file = self.new_property_file(target)?;
                self.property(Some(&file.partition), Some(&file.path), key)?
                    .into_iter()
                    .collect()
            }
            Some(PropertyTarget::Partition(partition)) => {
                self.partition(partition)?;
                self.property(Some(partition), None, key)?
                    .into_iter()
                    .collect()
            }
            None => {
                let mut found = Vec::new();
                for partition in self.precedence()? {
                    found.extend(self.property(Some(&partition), None, key)?);
                }
                found
            }
        };
        Ok(found
            .into_iter()
            .flat_map(|found| {
                let partition = found.partition;
                found.files.into_iter().map(move |path| Located {
                    place: Place::Build,
                    partition: partition.clone(),
                    path,
                })
            })
            .collect())
    }

    /// The file a new key goes to: the file; the partition's own `build.prop`;
    /// without a target, product's, whose properties load last of the standard
    /// partitions, so a new key there takes effect.
    pub fn new_property_file(&self, target: Option<&PropertyTarget>) -> Result<Located, Error> {
        let partition = match target {
            Some(PropertyTarget::File(path)) => {
                let mut found = Composition::resolve(self.targets, self.context.mounts, path);
                if found.len() != 1 {
                    return Err(Error::AbsentPath {
                        step: self.step.clone(),
                        path: path.clone(),
                    });
                }
                let found = found.remove(0);
                return Ok(Located {
                    place: Place::Build,
                    partition: found.partition,
                    path: found.path,
                });
            }
            Some(PropertyTarget::Partition(partition)) => {
                self.partition(partition)?;
                partition.clone()
            }
            None => match self.targets.keys().find(|name| name.as_ref() == "product") {
                Some(product) => product.clone(),
                None => self
                    .precedence()?
                    .into_iter()
                    .next()
                    .ok_or(Error::Unsupported("no partition to hold a new property"))?,
            },
        };
        let path = self
            .main_file(&partition)
            .ok_or_else(|| Error::NoPropertyFile(partition.clone()))?;
        Ok(Located {
            place: Place::Build,
            partition,
            path,
        })
    }

    /// The CIL files init compiles the device's SELinux policy from, in its order,
    /// that the build has: for the platform policy version the vendor names.
    fn policy_files(&self) -> Result<Vec<Located>, Error> {
        let [version] = &self.select(android_firmware::SEPOLICY_VERSION, &Range::default())?[..]
        else {
            return Err(Error::Unsupported(
                "the vendor does not name the platform policy version",
            ));
        };
        let version = String::from_utf8(self.content(version)?)
            .map_err(|_| Error::Unsupported("the platform policy version is UTF-8 text"))?;
        let mut found = Vec::new();
        for path in android_firmware::sepolicy_files(version.trim())? {
            found.extend(self.select(path.as_ref(), &Range::default())?);
        }
        Ok(found)
    }

    /// A partition the build must have.
    fn partition(&self, partition: &Name) -> Result<(), Error> {
        if self.targets.contains_key(partition) {
            return Ok(());
        }
        Err(Error::AbsentPartition {
            step: self.step.clone(),
            partition: partition.clone(),
        })
    }

    /// The one rule for each kind of expression.
    pub fn eval(&self, expr: &Expr) -> Result<Value, Error> {
        self.eval_in(expr, None)
    }

    fn eval_in(&self, expr: &Expr, bound: Option<&Bound<'_>>) -> Result<Value, Error> {
        Ok(match expr {
            Expr::Literal(value) => value.clone(),
            Expr::Select { pattern, scope } => Value::Selection(self.select(pattern, scope)?),
            Expr::Property { key, target } => self
                .get(target.as_ref(), key)?
                .map_or(Value::Unset, Value::Str),
            Expr::PropertyFiles { key, target } => {
                Value::Selection(self.property_files(key, target.as_ref())?)
            }
            Expr::PolicyFiles => Value::Selection(self.policy_files()?),
            Expr::NewPropertyFile(target) => {
                Value::Selection(vec![self.new_property_file(target.as_ref())?])
            }
            Expr::Var(name) => std::iter::successors(bound, |bound| bound.outer)
                .find(|bound| bound.name == name)
                .map(|bound| bound.value.clone())
                .ok_or(Error::Unsupported("a name no Let or Filter binds"))?,
            Expr::Let { name, value, body } => {
                let value = self.eval_in(value, bound)?;
                self.eval_in(body, Some(&Bound::new(name, value, bound)))?
            }
            Expr::Filter { over, name, keep } => {
                let holds = |value| {
                    let bound = Bound::new(name, value, bound);
                    Ok::<_, Error>(self.eval_in(keep, Some(&bound))?.as_bool()?)
                };
                match self.eval_in(over, bound)? {
                    Value::List(items) => {
                        let mut kept = Vec::new();
                        for item in items {
                            if holds(Value::Str(item.clone()))? {
                                kept.push(item);
                            }
                        }
                        Value::List(kept)
                    }
                    Value::Selection(entries) => {
                        let mut kept = Vec::new();
                        for entry in entries {
                            if holds(Value::Selection(vec![entry.clone()]))? {
                                kept.push(entry);
                            }
                        }
                        Value::Selection(kept)
                    }
                    _ => return Err(Error::Unsupported("Filter takes a list or a selection")),
                }
            }
            Expr::If {
                condition,
                then,
                otherwise,
            } => match self.eval_in(condition, bound)?.as_bool()? {
                true => self.eval_in(then, bound)?,
                false => self.eval_in(otherwise, bound)?,
            },
            Expr::Hash(inner) => {
                let selection = self.eval_in(inner, bound)?;
                let [entry] = selection.as_selection()? else {
                    return Err(Error::Unsupported("Hash needs exactly one file"));
                };
                if let (Place::Package, Some(package)) = (entry.place, self.package) {
                    let bytes = std::fs::read(entry.path.resolve(&package.decoded))?;
                    return Ok(Value::Str(
                        android_firmware::Sha256Digest::bytes(&bytes).to_string(),
                    ));
                }
                let target = self
                    .targets
                    .get(&entry.partition)
                    .ok_or_else(|| Error::UnknownPartition(entry.partition.clone()))?;
                Value::Str(target.file(&entry.path)?.sha256.to_string())
            }
            Expr::Below(inner) => {
                let mut found = Vec::new();
                for entry in self.eval_in(inner, bound)?.as_selection()? {
                    let tree = match entry.place {
                        Place::Build => {
                            &self
                                .targets
                                .get(&entry.partition)
                                .ok_or_else(|| Error::UnknownPartition(entry.partition.clone()))?
                                .tree
                        }
                        Place::Firmware(firmware) => {
                            let source = source_of(firmware, &entry.partition);
                            self.context
                                .sources
                                .get(&source)
                                .ok_or_else(|| Error::UnknownSource(source.clone()))?
                        }
                        Place::Package | Place::Asset => {
                            return Err(Error::Unsupported(
                                "Below selects in the build or a firmware",
                            ));
                        }
                    };
                    let prefix = format!("{}/", entry.path);
                    found.push(entry.clone());
                    found.extend(
                        tree.entries
                            .keys()
                            .filter(|path| path.as_ref().starts_with(&prefix))
                            .map(|path| Located {
                                place: entry.place,
                                partition: entry.partition.clone(),
                                path: path.clone(),
                            }),
                    );
                }
                found.sort();
                found.dedup();
                Value::Selection(found)
            }
            Expr::Read(inner) => {
                let selection = self.eval_in(inner, bound)?;
                let [entry] = selection.as_selection()? else {
                    return Err(Error::Unsupported("Read needs exactly one file"));
                };
                Value::Str(
                    String::from_utf8(self.content(entry)?)
                        .map_err(|_| Error::Unsupported("Read reads UTF-8 text"))?,
                )
            }
            Expr::Matches { text, pattern } => {
                let text = self.eval_in(text, bound)?;
                Value::Bool(text.as_str()?.is_some_and(|text| pattern.is_match(text)))
            }
            Expr::HasPartition(name) => Value::Bool(self.targets.contains_key(name)),
            Expr::Not(inner) => (!self.eval_in(inner, bound)?)?,
            Expr::Exists(inner) => self.eval_in(inner, bound)?.defined()?,
            Expr::Lines(inner) => self.eval_in(inner, bound)?.lines()?,
            Expr::Join(inner) => self.eval_in(inner, bound)?.join()?,
            // `All` and `Any` stop at the first condition that decides them.
            Expr::All(conditions) => {
                for condition in conditions {
                    if !self.eval_in(condition, bound)?.as_bool()? {
                        return Ok(Value::Bool(false));
                    }
                }
                Value::Bool(true)
            }
            Expr::Any(conditions) => {
                for condition in conditions {
                    if self.eval_in(condition, bound)?.as_bool()? {
                        return Ok(Value::Bool(true));
                    }
                }
                Value::Bool(false)
            }
            Expr::Union(selections) => {
                let mut union = Vec::new();
                for selection in selections {
                    union.extend_from_slice(self.eval_in(selection, bound)?.as_selection()?);
                }
                union.sort();
                union.dedup();
                Value::Selection(union)
            }
            Expr::Eq(left, right) => self
                .eval_in(left, bound)?
                .equals(&self.eval_in(right, bound)?)?,
            Expr::In(left, right) => self
                .eval_in(left, bound)?
                .is_in(&self.eval_in(right, bound)?)?,
        })
    }
}

impl Scope<'_> {
    /// The content of a selected file: in the build, a firmware or the open package.
    fn content(&self, entry: &Located) -> Result<Vec<u8>, Error> {
        match entry.place {
            Place::Asset => Ok(std::fs::read(entry.path.resolve(self.context.assets))?),
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
                let source = source_of(firmware, &entry.partition);
                let tree = self
                    .context
                    .sources
                    .get(&source)
                    .ok_or_else(|| Error::UnknownSource(source.clone()))?;
                Ok(android_fs::Store::sources(self.context.images)
                    .read(&tree.file(&entry.path)?)?)
            }
        }
    }
}

/// A build source, from a firmware and a partition.
pub(crate) fn source_of(from: Firmware, partition: &Name) -> SourceRef {
    SourceRef {
        source: from,
        partition: partition.clone(),
    }
}

/// A target's regular `.prop` files (or hardlinks to them), in path order.
fn property_files(target: &Target) -> Vec<RelativePath> {
    target
        .tree
        .entries
        .iter()
        .filter(|(path, entry)| {
            android_property::is_property_file(path.as_ref())
                && matches!(
                    entry.kind,
                    EntryKind::File { .. } | EntryKind::Hardlink { .. }
                )
        })
        .map(|(path, _)| path.clone())
        .collect()
}

/// A value a `Let` or `Filter` binds, and those bound around it.
struct Bound<'a> {
    name: &'a Name,
    value: Value,
    outer: Option<&'a Bound<'a>>,
}

impl<'a> Bound<'a> {
    fn new(name: &'a Name, value: Value, outer: Option<&'a Bound<'a>>) -> Self {
        Self { name, value, outer }
    }
}
