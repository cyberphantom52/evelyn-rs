//! What operations read and write: the entries a location or pattern selects in
//! the build, a firmware, the recipe's assets or the open package, their content,
//! and the property files init loads.

use crate::patch::{
    Composition, Error,
    compose::{Bound, Context},
    layout,
};
use crate::recipe::{
    Location,
    value::{Located, Place},
};
use android::EntryKind;
use android::firmware::{Firmware, SourceRef};
use android::partition::TargetPath;
use android::path::{DevicePath, Name, RelativePath};

impl Composition {
    /// What a location selects; a partition is the property files init loads
    /// from it.
    pub(crate) fn select(
        &self,
        location: &Location,
        bound: &Bound,
        context: &Context<'_>,
    ) -> Result<Vec<Located>, Error> {
        let path = crate::recipe::substitute(location.path(), bound)?;
        match location {
            Location::Build(_) => self.select_build(&path, None, None, context),
            Location::Rom(_) => self.select_firmware(Firmware::Rom, &path, context),
            Location::Stock(_) => self.select_firmware(Firmware::Stock, &path, context),
            Location::Asset(_) => self.select_asset(&path, context),
            Location::Package(_) => self.select_package(&path),
            Location::Partition(name) => Ok(self
                .loaded(context)?
                .files
                .into_iter()
                .filter(|file| file.partition == *name)
                .collect()),
        }
    }

    /// The files an operation's `target` names: in the open package, from its
    /// root, else in the build.
    pub(crate) fn named(
        &self,
        target: &str,
        bound: &Bound,
        context: &Context<'_>,
    ) -> Result<Vec<Located>, Error> {
        let target = crate::recipe::substitute(target, bound)?;
        match self.package {
            Some(_) => self.select_package(&target),
            None => self.select_build(&target, None, None, context),
        }
    }

    /// An asset, a file or directory, by its path in the recipe's directory.
    fn select_asset(&self, pattern: &str, context: &Context<'_>) -> Result<Vec<Located>, Error> {
        let path = RelativePath::try_from(pattern.to_owned())?;
        Ok(match context.has_asset(&path) {
            true => vec![Located {
                place: Place::Asset,
                partition: Name::try_from("assets".to_owned())?,
                path,
            }],
            false => Vec::new(),
        })
    }

    /// What a device path matches in a firmware.
    fn select_firmware(
        &self,
        firmware: Firmware,
        pattern: &str,
        context: &Context<'_>,
    ) -> Result<Vec<Located>, Error> {
        let mut found = Vec::new();
        for at in context
            .mounts
            .resolve(&DevicePath::try_from(pattern.to_owned())?)
        {
            let tree = context.source(&source_of(firmware, &at.partition))?;
            for path in layout::matches(tree, &at.path) {
                found.push(Located {
                    place: Place::Firmware(firmware),
                    partition: at.partition.clone(),
                    path,
                });
            }
        }
        Ok(found)
    }

    /// What a path from the open package's root matches in it.
    pub(crate) fn select_package(&self, pattern: &str) -> Result<Vec<Located>, Error> {
        let package = self.package.as_ref().ok_or(Error::Unsupported(
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
        found
            .into_iter()
            .map(|path| {
                Ok(Located {
                    place: Place::Package,
                    partition: package.partition.clone(),
                    path: RelativePath::try_from(path)?,
                })
            })
            .collect()
    }

    /// What a device path, or a relative pattern in the directory of each
    /// partition that came from `origin` and matches `partition` (a name or
    /// glob), matches in the build.
    pub(crate) fn select_build(
        &self,
        pattern: &str,
        origin: Option<Firmware>,
        partition: Option<&str>,
        context: &Context<'_>,
    ) -> Result<Vec<Located>, Error> {
        let paths = if pattern.starts_with('/') {
            Self::resolve(
                &self.targets,
                context.mounts,
                &DevicePath::try_from(pattern.to_owned())?,
            )
        } else {
            let selected: Vec<&Name> = self
                .targets
                .keys()
                .filter(|name| origin.is_none_or(|origin| self.origins.get(*name) == Some(&origin)))
                .filter(|name| {
                    partition.is_none_or(|glob| android::path::glob(glob, name.as_ref()))
                })
                .collect();
            if selected.is_empty() {
                return Err(Error::EmptyScope);
            }
            let mut paths = Vec::new();
            for name in selected {
                let Some(directory) = context.mounts.directory(name) else {
                    continue;
                };
                let device = DevicePath::try_from(format!("{directory}/{pattern}"))?;
                paths.extend(
                    Self::resolve(&self.targets, context.mounts, &device)
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

    /// The content of a selected file: in the build, a firmware, the assets or
    /// the open package.
    pub(crate) fn content(&self, entry: &Located, context: &Context<'_>) -> Result<Vec<u8>, Error> {
        match entry.place {
            Place::Asset => Ok(std::fs::read(context.asset(&entry.path)?)?),
            Place::Build => self
                .targets
                .get(&entry.partition)
                .ok_or_else(|| Error::UnknownPartition(entry.partition.clone()))?
                .read(&entry.path, context.images),
            Place::Package => {
                let package = self
                    .package
                    .as_ref()
                    .ok_or(Error::Unsupported("no package is open"))?;
                Ok(std::fs::read(entry.path.resolve(&package.decoded))?)
            }
            Place::Firmware(firmware) => {
                let tree = context.source(&source_of(firmware, &entry.partition))?;
                Ok(android::Store::sources(context.images).read(&tree.file(&entry.path)?)?)
            }
        }
    }

    /// The SHA-256 of a selected file.
    pub(crate) fn sha256(
        &self,
        entry: &Located,
        context: &Context<'_>,
    ) -> Result<android::hash::Sha256Digest, Error> {
        match entry.place {
            Place::Build => {
                let target = self
                    .targets
                    .get(&entry.partition)
                    .ok_or_else(|| Error::UnknownPartition(entry.partition.clone()))?;
                Ok(target
                    .store(context.images)
                    .sha256(&target.file(&entry.path)?)?)
            }
            _ => Ok(android::hash::Sha256Digest::bytes(
                &self.content(entry, context)?,
            )),
        }
    }

    /// The content of a selected file, as text.
    pub(crate) fn text(&self, entry: &Located, context: &Context<'_>) -> Result<String, Error> {
        String::from_utf8(self.content(entry, context)?)
            .map_err(|_| Error::Unsupported("text edits require UTF-8"))
    }

    /// Where a selected entry is on the device; in the open package, its path
    /// from the root.
    pub(crate) fn device_path(&self, entry: &Located, context: &Context<'_>) -> String {
        match entry.place {
            Place::Build | Place::Firmware(_) => context
                .mounts
                .device_path(&TargetPath {
                    partition: entry.partition.clone(),
                    path: entry.path.clone(),
                })
                .map_or_else(|| entry.path.to_string(), |path| path.to_string()),
            Place::Package => format!("/{}", entry.path),
            Place::Asset => entry.path.to_string(),
        }
    }

    /// Change the content of each file in the build or the open package to what
    /// `edit` makes of it; one in the build changes for its whole hardlink group.
    pub(crate) fn edit(
        &mut self,
        files: &[Located],
        context: &Context<'_>,
        mut edit: impl FnMut(&[u8]) -> Result<Vec<u8>, Error>,
    ) -> Result<(), Error> {
        for entry in files {
            match entry.place {
                Place::Build => {
                    let bytes = self.content(entry, context);
                    let edited = bytes.and_then(|bytes| {
                        let edited = edit(&bytes)?;
                        match edited == bytes {
                            true => Ok(()),
                            false => self.target(&entry.partition)?.write(&entry.path, &edited),
                        }
                    });
                    edited.map_err(|source| Error::At {
                        path: self.device_path(entry, context),
                        source: Box::new(source),
                    })?;
                }
                Place::Package => {
                    let edited = edit(&self.content(entry, context)?)?;
                    let package = self.package.as_ref().expect("a package's file is in it");
                    let path = entry.path.resolve(&package.decoded);
                    std::fs::write(&path, edited).map_err(crate::patch::package::io(&path))?;
                }
                _ => return Err(Error::Unsupported("only the build can be edited")),
            }
        }
        Ok(())
    }

    /// Edit the files `target` names, which must be there.
    pub(crate) fn edit_named(
        &mut self,
        target: &str,
        bound: &Bound,
        context: &Context<'_>,
        edit: impl FnMut(&[u8]) -> Result<Vec<u8>, Error>,
    ) -> Result<(), Error> {
        let files = self.named(target, bound, context)?;
        if files.is_empty() {
            let target = crate::recipe::substitute(target, bound)?;
            return Err(Error::Assertion(format!("{target} is not there")));
        }
        self.edit(&files, context, edit)
    }

    /// Remove a selected entry, and what is below it, from the build or the open
    /// package.
    pub(crate) fn remove_entry(&mut self, entry: &Located) -> Result<(), Error> {
        match (entry.place, &self.package) {
            (Place::Build, _) => self.target(&entry.partition)?.remove(&entry.path),
            (Place::Package, Some(package)) => {
                let path = entry.path.resolve(&package.decoded);
                let removed = match path.is_dir() {
                    true => std::fs::remove_dir_all(&path),
                    false => std::fs::remove_file(&path),
                };
                Ok(removed.map_err(crate::patch::package::io(&path))?)
            }
            _ => Err(Error::Unsupported("only the build can be removed from")),
        }
    }

    /// What init loads at boot: the property files, in order, the files their
    /// `import` lines name included, and the assignments it takes from them, in
    /// the order it takes them. An import whose path holds a `${property}` is not
    /// followed: only the device can expand it.
    pub(crate) fn loaded(&self, context: &Context<'_>) -> Result<Loaded, Error> {
        let mut loaded = Loaded::default();
        for group in android::property::PROPERTY_FILES {
            let mut files = Vec::new();
            for path in group {
                files.push(self.property_file(path, context)?);
            }
            let files: Vec<Located> = match files.remove(0) {
                Some(canonical) => vec![canonical],
                None => files.into_iter().flatten().collect(),
            };
            for file in &files {
                self.load(file, None, 0, &mut loaded, context)?;
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
        context: &Context<'_>,
    ) -> Result<(), Error> {
        if depth > 8 {
            return Err(Error::Unsupported("property imports nest more than 8 deep"));
        }
        if !loaded.files.contains(file) {
            loaded.files.push(file.clone());
        }
        let bytes = self.content(file, context)?;
        for line in android::property::lines(&String::from_utf8_lossy(&bytes)) {
            match line {
                android::property::Line::Import { path, filter: only } if filter.is_none() => {
                    if path.contains("${") {
                        continue;
                    }
                    if let Some(imported) = self.property_file(path, context)? {
                        self.load(&imported, only, depth + 1, loaded, context)?;
                    }
                }
                android::property::Line::Import { .. } => {}
                android::property::Line::Assignment(entry) => {
                    if android::property::loadable_property(entry.key)
                        && filter
                            .is_none_or(|filter| android::property::filtered(filter, entry.key))
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
    fn property_file(&self, path: &str, context: &Context<'_>) -> Result<Option<Located>, Error> {
        let path = DevicePath::try_from(path.to_owned())?;
        let found = Self::resolve(&self.targets, context.mounts, &path);
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
pub(crate) struct Loaded {
    /// The property files, in the order they are loaded.
    pub files: Vec<Located>,
    /// The assignments taken from them, in order: a later one of a key wins.
    pub assignments: Vec<(String, String)>,
}

impl Loaded {
    /// The value the device ends up with for `key`: init's last assignment of it,
    /// or for an `ro.product.*` property init derives, the one it derives.
    pub(crate) fn device_property(&self, key: &str) -> Option<String> {
        let value = |key: &str| {
            self.assignments
                .iter()
                .rev()
                .find(|(assigned, _)| assigned == key)
                .map(|(_, value)| value.clone())
        };
        let derived = key
            .strip_prefix("ro.product.")
            .filter(|name| android::property::DERIVED_PRODUCT_PROPERTIES.contains(name));
        let (Some(name), None | Some("")) = (derived, value(key).as_deref()) else {
            return value(key);
        };
        let sources = android::property::PRODUCT_PROPERTY_SOURCES;
        let order = value("ro.product.property_source_order").filter(|order| !order.is_empty());
        let order: Vec<String> = match order {
            Some(order) if order.split(',').all(|source| sources.contains(&source)) => {
                order.split(',').map(str::to_owned).collect()
            }
            _ => sources.iter().map(|source| source.to_string()).collect(),
        };
        order
            .iter()
            .find_map(|source| {
                value(&format!("ro.product.{source}.{name}")).filter(|value| !value.is_empty())
            })
            .or(value(key))
    }
}
