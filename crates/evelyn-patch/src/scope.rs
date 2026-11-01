//! What a recipe reads from the composed build and the firmware: selections,
//! property values and files, and the value of every expression.

use crate::{Composition, Error, Target, compose::Context, layout, package::Opened};
use android_firmware::{DevicePath, Firmware, Name, RelativePath, SourceRef};
use android_fs::EntryKind;
use recipe::{
    tree::{Expr, Scope as Range, Source},
    value::{Located, Place, Value},
};
use std::collections::BTreeMap;

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
    /// What init loads at boot: the property files, in order, and the assignments
    /// it takes from them, in the order it takes them.
    fn loaded(&self) -> Result<Loaded, Error> {
        let mut loaded = Loaded::default();
        for group in android_firmware::PROPERTY_FILES {
            let mut files = Vec::new();
            for path in group {
                files.push(self.property_file(path)?);
            }
            let files: Vec<Located> = match files.remove(0) {
                Some(canonical) => vec![canonical],
                None => files.into_iter().flatten().collect(),
            };
            for file in &files {
                self.load(file, None, 0, &mut loaded)?;
            }
        }
        Ok(loaded)
    }

    /// Load `file` as init does, following its imports; with `filter`, an import's,
    /// only the keys it lets in, and not the file's own imports.
    fn load(
        &self,
        file: &Located,
        filter: Option<&str>,
        depth: usize,
        loaded: &mut Loaded,
    ) -> Result<(), Error> {
        if depth > 8 {
            return Err(Error::Unsupported("property imports nest more than 8 deep"));
        }
        if !loaded.files.contains(file) {
            loaded.files.push(file.clone());
        }
        let bytes = self.content(file)?;
        for line in android_property::lines(&String::from_utf8_lossy(&bytes)) {
            match line {
                android_property::Line::Import { path, filter: only } if filter.is_none() => {
                    if path.contains("${") {
                        continue;
                    }
                    if let Some(imported) = self.property_file(path)? {
                        self.load(&imported, only, depth + 1, loaded)?;
                    }
                }
                android_property::Line::Import { .. } => {}
                android_property::Line::Assignment(entry) => {
                    if android_firmware::loadable_property(entry.key)
                        && filter.is_none_or(|filter| android_property::filtered(filter, entry.key))
                    {
                        loaded
                            .assignments
                            .push((entry.key.to_owned(), entry.value.to_owned()));
                    }
                }
            }
        }
        Ok(())
    }

    /// The regular file (or hardlink to one) at a device path in the build.
    fn property_file(&self, path: &str) -> Result<Option<Located>, Error> {
        let path = DevicePath::try_from(path.to_owned())?;
        let found = Composition::resolve(self.targets, self.context.mounts, &path);
        Ok(found.into_iter().find_map(|at| {
            let entry = self
                .targets
                .get(&at.partition)?
                .tree
                .entries
                .get(&at.path)?;
            matches!(
                entry.kind,
                EntryKind::File { .. } | EntryKind::Hardlink { .. }
            )
            .then(|| Located {
                place: Place::Build,
                partition: at.partition,
                path: at.path,
            })
        }))
    }

    /// The value the device ends up with for `key`.
    fn device_property(&self, key: &str) -> Result<Option<String>, Error> {
        let loaded = self.loaded()?;
        let value = |key: &str| {
            loaded
                .assignments
                .iter()
                .rev()
                .find(|(assigned, _)| assigned == key)
                .map(|(_, value)| value.clone())
        };
        let derived = key
            .strip_prefix("ro.product.")
            .filter(|name| android_firmware::DERIVED_PRODUCT_PROPERTIES.contains(name));
        let (Some(name), None | Some("")) = (derived, value(key).as_deref()) else {
            return Ok(value(key));
        };
        let sources = android_firmware::PRODUCT_PROPERTY_SOURCES;
        let order = value("ro.product.property_source_order").filter(|order| !order.is_empty());
        let order: Vec<String> = match order {
            Some(order) if order.split(',').all(|source| sources.contains(&source)) => {
                order.split(',').map(str::to_owned).collect()
            }
            _ => sources.iter().map(|source| source.to_string()).collect(),
        };
        Ok(order
            .iter()
            .find_map(|source| {
                value(&format!("ro.product.{source}.{name}")).filter(|value| !value.is_empty())
            })
            .or(value(key)))
    }

    /// What `pattern` selects. Inside a firmware, the pattern is a device path in
    /// it; inside the open package, a path from its root. In the build, a device
    /// path, or a relative pattern tried in the directory of every partition the
    /// range selects.
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
        if range.inside == Some(Source::Package) {
            let package = self.package.ok_or(Error::Unsupported(
                "a package's files are selected inside a Package",
            ))?;
            let path = pattern.strip_prefix('/').ok_or(Error::Unsupported(
                "paths inside a package start at its root, /",
            ))?;
            let found = layout::walk(
                path,
                |dir| {
                    std::fs::read_dir(package.decoded.join(dir))
                        .into_iter()
                        .flatten()
                        .flatten()
                        .map(|entry| entry.file_name().to_string_lossy().into_owned())
                        .collect()
                },
                |path| package.decoded.join(path).exists(),
            );
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

    /// The one rule for each kind of expression, with what the `Let`s and
    /// `Filter`s around it, in a statement or an expression, bind.
    pub fn eval(&self, expr: &Expr, bound: Option<&Bound<'_>>) -> Result<Value, Error> {
        Ok(match expr {
            Expr::Literal(value) => value.clone(),
            Expr::Select { pattern, scope } => {
                let pattern = self.eval(pattern, bound)?;
                let pattern = pattern
                    .as_str()?
                    .ok_or(Error::Unsupported("Select needs a pattern that is set"))?;
                Value::Selection(self.select(pattern, scope)?)
            }
            Expr::Property { key, files: None } => {
                self.device_property(key)?.map_or(Value::Unset, Value::Str)
            }
            Expr::Property {
                key,
                files: Some(files),
            } => {
                let mut value = Value::Unset;
                for file in self.eval(files, bound)?.as_selection()? {
                    let text = String::from_utf8_lossy(&self.content(file)?).into_owned();
                    if let Some(assigned) = android_property::get(&text, key) {
                        value = Value::Str(assigned.to_owned());
                    }
                }
                value
            }
            Expr::PropertyFiles { partition } => Value::Selection(
                self.loaded()?
                    .files
                    .into_iter()
                    .filter(|file| {
                        partition
                            .as_ref()
                            .is_none_or(|name| file.partition == *name)
                    })
                    .collect(),
            ),
            Expr::Var(name) => std::iter::successors(bound, |bound| bound.outer)
                .find(|bound| bound.name == name)
                .map(|bound| bound.value.clone())
                .ok_or(Error::Unsupported("a name no Let or Filter binds"))?,
            Expr::Let { name, value, body } => {
                let value = self.eval(value, bound)?;
                self.eval(body, Some(&Bound::new(name, value, bound)))?
            }
            Expr::Filter { over, name, keep } => {
                let holds = |value| {
                    let bound = Bound::new(name, value, bound);
                    Ok::<_, Error>(self.eval(keep, Some(&bound))?.as_bool()?)
                };
                match self.eval(over, bound)? {
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
            } => match self.eval(condition, bound)?.as_bool()? {
                true => self.eval(then, bound)?,
                false => self.eval(otherwise, bound)?,
            },
            Expr::Hash(inner) => {
                let selection = self.eval(inner, bound)?;
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
                for entry in self.eval(inner, bound)?.as_selection()? {
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
                let selection = self.eval(inner, bound)?;
                let [entry] = selection.as_selection()? else {
                    return Err(Error::Unsupported("Read needs exactly one file"));
                };
                Value::Str(
                    String::from_utf8(self.content(entry)?)
                        .map_err(|_| Error::Unsupported("Read reads UTF-8 text"))?,
                )
            }
            Expr::Matches { text, pattern } => {
                let text = self.eval(text, bound)?;
                Value::Bool(text.as_str()?.is_some_and(|text| pattern.is_match(text)))
            }
            Expr::HasPartition(name) => Value::Bool(self.targets.contains_key(name)),
            Expr::Not(inner) => (!self.eval(inner, bound)?)?,
            Expr::Exists(inner) => self.eval(inner, bound)?.defined()?,
            Expr::Concat(parts) => {
                let mut text = String::new();
                for part in parts {
                    let part = self.eval(part, bound)?;
                    text.push_str(
                        part.as_str()?
                            .ok_or(Error::Unsupported("Concat needs strings that are set"))?,
                    );
                }
                Value::Str(text)
            }
            Expr::Trim(inner) => match self.eval(inner, bound)? {
                Value::Str(text) => Value::Str(text.trim().to_owned()),
                other => other.as_str().map(|_| other.clone())?,
            },
            Expr::Lines(inner) => self.eval(inner, bound)?.lines()?,
            Expr::Join(inner) => self.eval(inner, bound)?.join()?,
            // `All` and `Any` stop at the first condition that decides them.
            Expr::All(conditions) => {
                for condition in conditions {
                    if !self.eval(condition, bound)?.as_bool()? {
                        return Ok(Value::Bool(false));
                    }
                }
                Value::Bool(true)
            }
            Expr::Any(conditions) => {
                for condition in conditions {
                    if self.eval(condition, bound)?.as_bool()? {
                        return Ok(Value::Bool(true));
                    }
                }
                Value::Bool(false)
            }
            Expr::Union(selections) => {
                let mut union = Vec::new();
                for selection in selections {
                    union.extend_from_slice(self.eval(selection, bound)?.as_selection()?);
                }
                let mut seen = std::collections::BTreeSet::new();
                union.retain(|entry| seen.insert(entry.clone()));
                Value::Selection(union)
            }
            Expr::Eq(left, right) => self.eval(left, bound)?.equals(&self.eval(right, bound)?)?,
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

/// What init loads at boot.
#[derive(Default)]
struct Loaded {
    /// The property files, in the order they are loaded.
    files: Vec<Located>,
    /// The assignments taken from them, in order: a later one of a key wins.
    assignments: Vec<(String, String)>,
}

/// A value a `Let` (a statement's or an expression's) or a `Filter` binds, and
/// those bound around it.
pub(crate) struct Bound<'a> {
    name: &'a Name,
    value: Value,
    outer: Option<&'a Bound<'a>>,
}

impl<'a> Bound<'a> {
    pub(crate) fn new(name: &'a Name, value: Value, outer: Option<&'a Bound<'a>>) -> Self {
        Self { name, value, outer }
    }
}
