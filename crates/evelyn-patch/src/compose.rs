//! Apply recipes across every target partition. A recipe is desugared into its
//! primitive tree, and each statement and expression has one implementation here
//! and in [`crate::scope`], whatever surface operation it came from.

use crate::{
    Error, Images, Target,
    defaults::Defaults,
    layout,
    operations::{Pending, Placing},
    package::Opened,
    scope::{Bound, Scope},
};
use android_firmware::{
    DevicePath, Firmware, Mounts, Name, RelativePath, STANDARD_SYSTEM, SourceRef, TargetPath,
    VENDOR_SIDE,
};
use android_fs::PartitionTree;
use recipe::{
    tree::{Assigned, Expr, Metadata, Overrides, Stmt, Transform, Tree},
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
    /// The package a `Package` has open.
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

    /// Apply a recipe's primitive tree, its steps in dependency order.
    pub fn apply(&mut self, tree: &Tree, context: &Context<'_>) -> Result<(), Error> {
        let unnamed = Name::try_from("recipe".to_owned())?;
        for stmt in &tree.body {
            self.exec(stmt, &tree.name, &unnamed, context, None)?;
        }
        Ok(())
    }

    /// `expr`'s value, with what the `Let`s around the statement bind.
    fn eval(
        &self,
        expr: &Expr,
        context: &Context<'_>,
        step: &Name,
        bound: Option<&Bound<'_>>,
    ) -> Result<Value, Error> {
        Scope {
            targets: &self.targets,
            origins: &self.origins,
            context,
            step,
            package: self.package.as_ref(),
        }
        .eval(expr, bound)
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
        bound: Option<&Bound<'_>>,
    ) -> Result<(), Error> {
        if self.package.is_some() && matches!(stmt, Stmt::SetMetadata { .. } | Stmt::Package { .. })
        {
            return Err(Error::Unsupported(
                "inside a package only Edit, Copy, Remove, Let, When and Assert run",
            ));
        }
        match stmt {
            Stmt::Step { id, body } => {
                for stmt in body {
                    self.exec(stmt, recipe, id, context, bound)?;
                }
            }
            Stmt::Let { name, value, body } => {
                let value = self.eval(value, context, step, bound)?;
                if let Some(text) = value.as_str()?
                    && !android_firmware::is_token(text)
                {
                    return Err(recipe::Error::Invalid(format!(
                        "{name} is bound to {text:?}, which is not [A-Za-z0-9._-]"
                    ))
                    .into());
                }
                let bound = Bound::new(name, value, bound);
                for stmt in body {
                    self.exec(stmt, recipe, step, context, Some(&bound))?;
                }
            }
            Stmt::When {
                condition,
                body,
                skip,
            } => {
                if self.eval(condition, context, step, bound)?.as_bool()? {
                    for stmt in body {
                        self.exec(stmt, recipe, step, context, bound)?;
                    }
                } else {
                    self.skip(recipe, step, skip.clone());
                }
            }
            Stmt::Remove { selection } => {
                let selected = self.eval(selection, context, step, bound)?;
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
            Stmt::Copy {
                selection,
                to,
                metadata,
                overwrite,
                create_parent,
            } => {
                let placing = Placing {
                    metadata: *metadata,
                    overwrite: *overwrite,
                    create_parent: *create_parent,
                };
                let selected = self.eval(selection, context, step, bound)?;
                let selected = selected.as_selection()?;
                let to = match to {
                    Some(to) => {
                        let to = self.eval(to, context, step, bound)?;
                        let to = to
                            .as_str()?
                            .ok_or(Error::Unsupported("a copy's to is a string that is set"))?;
                        Some(DevicePath::try_from(to.to_owned())?)
                    }
                    None => None,
                };
                if to.is_some() && selected.len() > 1 {
                    return Err(Error::Unsupported(
                        "a copy of several entries keeps their paths",
                    ));
                }
                for entry in selected {
                    match entry.place {
                        Place::Firmware(firmware) => self.copy_firmware(
                            firmware,
                            entry,
                            to.as_ref(),
                            placing,
                            context,
                            step,
                        )?,
                        Place::Asset => {
                            let to = to
                                .as_ref()
                                .ok_or(Error::Unsupported("an asset is copied to a path"))?;
                            self.copy_asset(&entry.path, to, placing, context, step)?;
                        }
                        Place::Build => {
                            let to = to.as_ref().ok_or(Error::Unsupported(
                                "a copy within the build needs a path",
                            ))?;
                            self.copy_build(entry, to, placing, context, step)?;
                        }
                        Place::Package => {
                            return Err(Error::Unsupported("a package's files are not copied"));
                        }
                    }
                }
            }
            Stmt::SetMetadata {
                selection,
                pattern,
                metadata,
            } => {
                let selected = self.eval(selection, context, step, bound)?;
                let selected = selected.as_selection()?;
                let overrides = match metadata {
                    Assigned::Default => {
                        for entry in selected {
                            if entry.place != Place::Build {
                                return Err(Error::Unsupported(
                                    "only the build's entries get Android's defaults",
                                ));
                            }
                            self.target(&entry.partition)?.reset(&entry.path);
                        }
                        return Ok(());
                    }
                    // The label is evaluated before it is set.
                    Assigned::Fields(overrides) => &Overrides {
                        selinux: match &overrides.selinux {
                            Some(label) => {
                                let label = self.eval(label, context, step, bound)?;
                                let label = label
                                    .as_str()?
                                    .filter(|label| recipe::tree::selinux_label(label))
                                    .ok_or(Error::Unsupported("a label is a well-formed string"))?;
                                Some(Expr::Literal(Value::Str(label.to_owned())))
                            }
                            None => None,
                        },
                        ..overrides.clone()
                    },
                };
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
                let selected = self.eval(selection, context, step, bound)?;
                let selected = selected.as_selection()?;
                if selected.is_empty() {
                    return Ok(());
                }
                // A transform's strings, and the policy files a policy patch resolves
                // with, are expressions, evaluated before the edit.
                let transform = transform.map_strings(|text| {
                    let text = self.eval(text, context, step, bound)?;
                    let text = text
                        .as_str()?
                        .ok_or(Error::Unsupported("a transform's strings are set"))?;
                    Ok::<_, Error>(Expr::Literal(Value::Str(text.to_owned())))
                })?;
                let transform = &match transform {
                    Transform::AddPolicy {
                        policies,
                        resolve_with,
                    } => {
                        let mut texts = Vec::new();
                        for file in self
                            .eval(&resolve_with, context, step, bound)?
                            .as_selection()?
                        {
                            let file = Expr::Literal(Value::Selection(vec![file.clone()]));
                            let text =
                                self.eval(&Expr::Read(Box::new(file)), context, step, bound)?;
                            texts.extend(text.as_str()?.map(str::to_owned));
                        }
                        Transform::AddPolicy {
                            policies,
                            resolve_with: Expr::Literal(Value::List(texts)),
                        }
                    }
                    other => other,
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
                                crate::operations::transformed(&bytes, transform, context)?;
                            std::fs::write(&path, changed).map_err(crate::package::io(&path))?;
                        }
                        _ => return Err(Error::Unsupported("only the build can be edited")),
                    }
                }
            }
            Stmt::Assert { condition, message } => {
                if !self.eval(condition, context, step, bound)?.as_bool()? {
                    let message = self.eval(message, context, step, bound)?;
                    return Err(Error::Assertion {
                        step: step.clone(),
                        message: message.as_str()?.unwrap_or_default().to_owned(),
                    });
                }
            }
            Stmt::Package { package, body } => {
                let package = self.eval(package, context, step, bound)?;
                let package = package.as_str()?.ok_or(Error::Unsupported(
                    "a package's path is a string that is set",
                ))?;
                let at =
                    self.resolve_one(&DevicePath::try_from(package.to_owned())?, context, step)?;
                let opened =
                    crate::package::open(self.target(&at.partition)?, &at, body, context, step)?;
                self.package = Some(opened);
                let ran = body
                    .iter()
                    .try_for_each(|stmt| self.exec(stmt, recipe, step, context, bound));
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
        placing: Placing,
        context: &Context<'_>,
        step: &Name,
    ) -> Result<(), Error> {
        if self.package.is_some() {
            return Err(Error::Unsupported(
                "a package takes assets, not firmware files",
            ));
        }
        let source = crate::scope::source_of(firmware, &entry.partition);
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
        let target = self.target(&to.partition)?;
        let written = target.import(tree, &source, &entry.path, &to.path, placing, step)?;
        target.copied(written, placing.metadata, Vec::new());
        Ok(())
    }

    /// Copy a recipe asset (a file, or a directory's files) to `to`, in the build
    /// or the open package. An asset has no metadata of its own.
    fn copy_asset(
        &mut self,
        asset: &RelativePath,
        to: &DevicePath,
        placing: Placing,
        context: &Context<'_>,
        step: &Name,
    ) -> Result<(), Error> {
        let source = asset.resolve(context.assets);
        if let Some(package) = &self.package {
            let path =
                RelativePath::try_from(to.to_string()[1..].to_owned())?.resolve(&package.decoded);
            let inside = RelativePath::try_from(to.to_string()[1..].to_owned())?;
            if path.exists() && !placing.overwrite {
                return Err(layout::Error::DestinationExists(inside).into());
            }
            if let Some(parent) = path.parent() {
                if !parent.exists() && !placing.create_parent {
                    return Err(layout::Error::MissingParent(inside).into());
                }
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
            toolchain::files(&source)?
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
        if placing.metadata == Metadata::Source {
            return Err(Error::Unsupported(
                "an asset has no metadata of its own to keep",
            ));
        }
        let target = self.target(&partition)?;
        let mut written = BTreeSet::new();
        for (to, file, asset) in files {
            let changed = target.install(&file, &asset, &to, placing)?;
            written.extend(target.accept(changed, step));
        }
        target.copied(written, placing.metadata, Vec::new());
        Ok(())
    }

    /// Copy an entry of the build, and what is below it, to `to`, keeping their
    /// metadata; those still waiting for Android's defaults wait at the copy too.
    fn copy_build(
        &mut self,
        entry: &Located,
        to: &DevicePath,
        placing: Placing,
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
        let written = if entry.partition == to.partition {
            self.target(&to.partition)?
                .copy_within(&entry.path, &to.path, placing, step)?
        } else {
            let source = self
                .targets
                .remove(&entry.partition)
                .ok_or_else(|| Error::UnknownPartition(entry.partition.clone()))?;
            let copied = self.target(&to.partition).and_then(|destination| {
                source.copy_into(destination, &entry.path, &to.path, placing, step)
            });
            self.targets.insert(entry.partition.clone(), source);
            copied?
        };
        self.target(&to.partition)?
            .copied(written, placing.metadata, waiting);
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
    /// them; write every target's manifest and return the finished trees, with the
    /// steps that were skipped and the `SetMetadata` that changed nothing.
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
