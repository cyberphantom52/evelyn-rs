//! Apply recipes across every target partition. A recipe is desugared into its
//! primitive tree, and each statement and expression has one implementation here
//! and in [`crate::scope`], whatever surface operation it came from.

use crate::{
    Error, Images, Target, defaults::Defaults, operations::Pending, package::Opened, scope::Scope,
};
use model::{DevicePath, Mounts, Name, PartitionTree, RelativePath, SourceRef, TargetPath};
use recipe::{
    Firmware, Recipe, STANDARD_SYSTEM, VENDOR_SIDE,
    tree::{Expr, Stmt, Transform},
    value::{Located, Place, Value},
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
use toolchain::Tools;

/// Scanned source trees, by source reference.
pub type Sources = BTreeMap<SourceRef, PartitionTree>;

/// What operations need beyond the targets themselves.
pub struct Context<'a> {
    pub sources: &'a Sources,
    /// The open source images that untouched file contents are read from.
    pub images: &'a Images,
    pub tools: &'a Tools,
    /// The recipe's directory; asset paths resolve below it.
    pub assets: &'a Path,
    /// Scratch directory for decoded packages.
    pub packages: &'a Path,
    /// Where each partition is mounted, for resolving device paths.
    pub mounts: &'a Mounts,
}

/// A `SetMetadata` that set what was already there.
#[derive(Clone, Debug, Serialize)]
pub struct Unchanged {
    pub step: Name,
    pub pattern: String,
}

/// A step, or part of one, that did not apply, and why.
#[derive(Clone, Debug, Serialize)]
pub struct Skipped {
    pub recipe: Name,
    pub step: Name,
    pub reason: String,
}

pub struct Composition {
    targets: BTreeMap<Name, Target>,
    /// Which firmware each target partition was built from.
    origins: BTreeMap<Name, Firmware>,
    skipped: Vec<Skipped>,
    /// Every `SetMetadata` statement, and whether it changed anything.
    set: Vec<(Unchanged, bool)>,
    /// The package a `Within` has open.
    package: Option<Opened>,
}

/// What a composition reports besides the trees.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    pub skipped: Vec<Skipped>,
    /// `SetMetadata` patterns that changed nothing: what they set is already there.
    pub unchanged: Vec<Unchanged>,
}

impl Composition {
    pub fn new(targets: BTreeMap<Name, Target>, origins: BTreeMap<Name, Firmware>) -> Self {
        Self {
            targets,
            origins,
            skipped: Vec::new(),
            set: Vec::new(),
            package: None,
        }
    }

    /// Apply a recipe: its primitive tree, steps in dependency order.
    pub fn apply(&mut self, recipe: &Recipe, context: &Context<'_>) -> Result<(), Error> {
        let tree = recipe.tree()?;
        let unnamed = Name::try_from("recipe".to_owned())?;
        for stmt in &tree.body {
            self.exec(stmt, &tree.name, &unnamed, context)?;
        }
        Ok(())
    }

    fn eval(&self, expr: &Expr, context: &Context<'_>, step: &Name) -> Result<Value, Error> {
        Scope {
            targets: &self.targets,
            origins: &self.origins,
            context,
            step,
            package: self.package.as_ref(),
        }
        .eval(expr)
    }

    fn skip(&mut self, recipe: &Name, step: &Name, reason: String) {
        self.skipped.push(Skipped {
            recipe: recipe.clone(),
            step: step.clone(),
            reason,
        });
    }

    /// The one rule for each kind of statement.
    fn exec(
        &mut self,
        stmt: &Stmt,
        recipe: &Name,
        step: &Name,
        context: &Context<'_>,
    ) -> Result<(), Error> {
        if self.package.is_some()
            && matches!(
                stmt,
                Stmt::Reset { .. } | Stmt::SetMetadata { .. } | Stmt::Within { .. }
            )
        {
            return Err(Error::Unsupported(
                "inside a package only Edit, Write, Delete, Let, When and Assert run",
            ));
        }
        match stmt {
            Stmt::Step { id, body } => {
                for stmt in body {
                    self.exec(stmt, recipe, id, context)?;
                }
            }
            Stmt::Let { name, value, body } => {
                let bound = self.eval(value, context, step)?;
                match bound.as_str()? {
                    None => self.skip(recipe, step, format!("{name} is not set: {}", ron(value))),
                    Some(bound) => {
                        let bindings = BTreeMap::from([(name.clone(), bound.to_owned())]);
                        for stmt in &recipe::substitute(body, &bindings)? {
                            self.exec(stmt, recipe, step, context)?;
                        }
                    }
                }
            }
            Stmt::When {
                condition,
                body,
                skip,
            } => {
                if self.eval(condition, context, step)?.as_bool()? {
                    for stmt in body {
                        self.exec(stmt, recipe, step, context)?;
                    }
                } else {
                    self.skip(recipe, step, skip.clone());
                }
            }
            Stmt::Delete { selection } => {
                let selected = self.eval(selection, context, step)?;
                let selected = selected.as_selection()?;
                for entry in selected {
                    match (entry.place, &self.package) {
                        (Place::Build, _) => {
                            self.target(&entry.partition)?.remove(&entry.path, step)?;
                        }
                        (Place::Package, Some(package)) => {
                            let path = entry.path.resolve(&package.decoded);
                            let removed = match path.is_dir() {
                                true => std::fs::remove_dir_all(&path),
                                false => std::fs::remove_file(&path),
                            };
                            removed.map_err(crate::package::io(&path))?;
                        }
                        _ => return Err(Error::Unsupported("only the build can be removed from")),
                    }
                }
            }
            Stmt::Copy { selection, to } => {
                let selected = self.eval(selection, context, step)?;
                let selected = selected.as_selection()?;
                if to.is_some() && selected.len() > 1 {
                    return Err(Error::Unsupported(
                        "a copy of several entries keeps their paths",
                    ));
                }
                for entry in selected {
                    match entry.place {
                        Place::Firmware(firmware) => {
                            self.copy_firmware(firmware, entry, to.as_ref(), context, step)?
                        }
                        Place::Asset => {
                            let to = to
                                .as_ref()
                                .ok_or(Error::Unsupported("an asset is copied to a path"))?;
                            self.copy_asset(&entry.path, to, context, step)?;
                        }
                        Place::Build => {
                            let to = to.as_ref().ok_or(Error::Unsupported(
                                "a copy within the build needs a path",
                            ))?;
                            self.copy_build(entry, to, context, step)?;
                        }
                        Place::Package => {
                            return Err(Error::Unsupported("a package's files are not copied"));
                        }
                    }
                }
            }
            Stmt::Reset { selection } => {
                let selected = self.eval(selection, context, step)?;
                let selected = selected.as_selection()?;
                for entry in selected {
                    if entry.place != Place::Build {
                        return Err(Error::Unsupported("only the build's entries can be reset"));
                    }
                    self.target(&entry.partition)?.reset(&entry.path);
                }
            }
            Stmt::SetMetadata {
                selection,
                pattern,
                overrides,
            } => {
                let selected = self.eval(selection, context, step)?;
                let selected = selected.as_selection()?;
                let statement = self.set.len();
                self.set.push((
                    Unchanged {
                        step: step.clone(),
                        pattern: pattern.clone(),
                    },
                    false,
                ));
                for entry in selected {
                    let changed = self.target(&entry.partition)?.set_metadata(
                        &entry.path,
                        overrides,
                        statement,
                        step,
                    )?;
                    if changed == Some(true) {
                        self.set[statement].1 = true;
                    }
                }
            }
            Stmt::Edit {
                selection,
                transform,
            } => {
                let selected = self.eval(selection, context, step)?;
                let selected = selected.as_selection()?;
                // Append's string is an expression, evaluated before the edit.
                let transform = &match transform {
                    Transform::Append(text) => {
                        let text = self.eval(text, context, step)?;
                        let text = text
                            .as_str()?
                            .ok_or(Error::Unsupported("Append needs a string that is set"))?;
                        Transform::Append(Expr::Literal(Value::Str(text.to_owned())))
                    }
                    other => other.clone(),
                };
                for entry in selected {
                    match (entry.place, &self.package) {
                        (Place::Build, _) => self
                            .target(&entry.partition)?
                            .transform(&entry.path, transform, context, step)
                            .map_err(|source| Error::At {
                                path: context
                                    .mounts
                                    .device_path(&TargetPath {
                                        partition: entry.partition.clone(),
                                        path: entry.path.clone(),
                                    })
                                    .map_or_else(
                                        || entry.path.to_string(),
                                        |path| path.to_string(),
                                    ),
                                source: Box::new(source),
                            })?,
                        (Place::Package, Some(package)) => {
                            let path = entry.path.resolve(&package.decoded);
                            let bytes = std::fs::read(&path).map_err(crate::package::io(&path))?;
                            let changed =
                                crate::operations::transformed(&bytes, transform, context.assets)?;
                            std::fs::write(&path, changed).map_err(crate::package::io(&path))?;
                        }
                        _ => return Err(Error::Unsupported("only the build can be edited")),
                    }
                }
            }
            Stmt::Assert { condition, message } => {
                if !self.eval(condition, context, step)?.as_bool()? {
                    return Err(Error::Assertion {
                        step: step.clone(),
                        message: message.clone(),
                    });
                }
            }
            Stmt::Within {
                package,
                before,
                body,
            } => {
                let at = self.resolve_one(package, context, step)?;
                let opened = crate::package::open(
                    self.target(&at.partition)?,
                    &at,
                    before.as_ref(),
                    body,
                    context,
                    step,
                )?;
                self.package = Some(opened);
                let ran = body
                    .iter()
                    .try_for_each(|stmt| self.exec(stmt, recipe, step, context));
                let opened = self.package.take().expect("opened above");
                ran?;
                crate::package::close(
                    self.target(&at.partition)?,
                    &at.path,
                    opened,
                    context,
                    step,
                )?;
            }
        }
        Ok(())
    }

    /// Copy what a firmware has at `entry` into the build.
    fn copy_firmware(
        &mut self,
        firmware: Firmware,
        entry: &Located,
        to: Option<&DevicePath>,
        context: &Context<'_>,
        step: &Name,
    ) -> Result<(), Error> {
        if self.package.is_some() {
            return Err(Error::Unsupported(
                "a package takes assets, not firmware files",
            ));
        }
        let source = crate::scope::source_of(firmware, &entry.partition)?;
        let tree = context
            .sources
            .get(&source)
            .ok_or_else(|| Error::UnknownSource(source.clone()))?;
        let to = match to {
            Some(to) => self.resolve_one(to, context, step)?,
            None => TargetPath {
                partition: entry.partition.clone(),
                path: entry.path.clone(),
            },
        };
        self.target(&to.partition)?
            .import(tree, &source, &entry.path, &to.path, step)
    }

    /// Copy a recipe asset (a file, or a directory's files) to `to`, in the build
    /// or the open package. New files get Android's defaults.
    fn copy_asset(
        &mut self,
        asset: &RelativePath,
        to: &DevicePath,
        context: &Context<'_>,
        step: &Name,
    ) -> Result<(), Error> {
        let source = asset.resolve(context.assets);
        if let Some(package) = &self.package {
            let path =
                RelativePath::try_from(to.to_string()[1..].to_owned())?.resolve(&package.decoded);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(crate::package::io(parent))?;
            }
            std::fs::copy(&source, &path).map_err(crate::package::io(&source))?;
            return Ok(());
        }
        // A partition's mount point is its root; anything else is a path in one.
        let (partition, base) = match context.mounts.partition_at(to) {
            Some(partition) => (partition.clone(), None),
            None => {
                let at = self.resolve_one(to, context, step)?;
                (at.partition, Some(at.path))
            }
        };
        let below = |path: &str| match &base {
            Some(base) => RelativePath::try_from(format!("{base}/{path}")),
            None => RelativePath::try_from(path.to_owned()),
        };
        let files = if source.is_dir() {
            crate::operations::files(&source)?
                .into_iter()
                .map(|(relative, file)| {
                    Ok((
                        below(&relative)?,
                        file,
                        RelativePath::try_from(format!("{asset}/{relative}"))?,
                    ))
                })
                .collect::<Result<Vec<_>, Error>>()?
        } else {
            let to = base.ok_or(Error::Unsupported(
                "a file cannot replace a partition's root",
            ))?;
            vec![(to, source, asset.clone())]
        };
        let target = self.target(&partition)?;
        for (to, file, asset) in files {
            let changed = target.install(&file, &asset, &to)?;
            target.accept(changed, step);
        }
        Ok(())
    }

    /// Copy an entry of the build, and what is below it, to `to`, keeping their
    /// metadata; those still waiting for Android's defaults wait at the copy too.
    fn copy_build(
        &mut self,
        entry: &Located,
        to: &DevicePath,
        context: &Context<'_>,
        step: &Name,
    ) -> Result<(), Error> {
        let to = self.resolve_one(to, context, step)?;
        let below = |path: &RelativePath, root: &RelativePath| {
            path == root || path.as_ref().starts_with(&format!("{root}/"))
        };
        let waiting: Vec<(RelativePath, Vec<Pending>)> = self
            .target(&entry.partition)?
            .derived
            .iter()
            .filter(|(path, _)| below(path, &entry.path))
            .map(|(path, pending)| {
                let copied = format!("{}{}", to.path, &path.as_ref()[entry.path.as_ref().len()..]);
                Ok((RelativePath::try_from(copied)?, pending.clone()))
            })
            .collect::<Result<_, Error>>()?;
        let before: BTreeSet<RelativePath> = self
            .target(&to.partition)?
            .derived
            .keys()
            .cloned()
            .collect();
        if entry.partition == to.partition {
            self.target(&to.partition)?
                .copy_within(&entry.path, &to.path, step)?;
        } else {
            let source = self
                .targets
                .remove(&entry.partition)
                .ok_or_else(|| Error::UnknownPartition(entry.partition.clone()))?;
            let copied = self
                .target(&to.partition)
                .and_then(|destination| source.copy_into(destination, &entry.path, &to.path, step));
            self.targets.insert(entry.partition.clone(), source);
            copied?;
        }
        // What the copy made is not new: it has its source's metadata.
        let destination = self.target(&to.partition)?;
        destination.derived.retain(|path, _| before.contains(path));
        destination.derived.extend(waiting);
        Ok(())
    }

    /// Where a device path is in the build. A literal path under a standard mount
    /// point whose partition this build lacks (`/odm/...` without `odm`) is nowhere,
    /// rather than in the empty mount-point directory `system` has for it.
    pub(crate) fn resolve(
        targets: &BTreeMap<Name, Target>,
        mounts: &Mounts,
        path: &DevicePath,
    ) -> Vec<TargetPath> {
        let first = path.components().next().unwrap_or_default();
        let standard = VENDOR_SIDE.contains(&first) || STANDARD_SYSTEM.contains(&first);
        if standard && first != "system" && !targets.keys().any(|name| name.as_ref() == first) {
            return Vec::new();
        }
        mounts.resolve(path)
    }

    /// The one place a device path is in the build.
    fn resolve_one(
        &self,
        path: &DevicePath,
        context: &Context<'_>,
        step: &Name,
    ) -> Result<TargetPath, Error> {
        let mut found = Self::resolve(&self.targets, context.mounts, path);
        match found.len() {
            1 => Ok(found.remove(0)),
            0 => Err(Error::AbsentPath {
                step: step.clone(),
                path: path.clone(),
            }),
            _ => Err(Error::Unsupported(
                "a path to edit must name one partition, not a glob over several",
            )),
        }
    }

    fn target(&mut self, name: &Name) -> Result<&mut Target, Error> {
        self.targets
            .get_mut(name)
            .ok_or_else(|| Error::UnknownPartition(name.clone()))
    }

    /// Give the entries waiting for them Android's defaults, from the finished
    /// build's own policy as the device labels with it, then what was set over
    /// them; write
    /// every target's manifest and return the finished trees, with the steps that
    /// were skipped, the removed patterns that matched nothing and the
    /// `SetMetadata` that changed nothing.
    pub fn finish(
        mut self,
        mounts: &Mounts,
        images: &Images,
    ) -> Result<(BTreeMap<Name, PartitionTree>, Report), Error> {
        if self
            .targets
            .values()
            .any(|target| !target.derived.is_empty())
        {
            let defaults = Defaults::new(&self.targets, mounts, images)?;
            for (name, target) in &mut self.targets {
                for statement in target.give_defaults(name, mounts, &defaults)? {
                    self.set[statement].1 = true;
                }
            }
        }
        let unchanged = self
            .set
            .into_iter()
            .filter(|(_, changed)| !changed)
            .map(|(statement, _)| statement)
            .collect();
        let trees = self
            .targets
            .into_iter()
            .map(|(name, target)| Ok((name, target.finish()?)))
            .collect::<Result<_, Error>>()?;
        Ok((
            trees,
            Report {
                skipped: self.skipped,
                unchanged,
            },
        ))
    }
}

fn ron(value: &impl Serialize) -> String {
    ron::to_string(value).unwrap_or_default()
}
