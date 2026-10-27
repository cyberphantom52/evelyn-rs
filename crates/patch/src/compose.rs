//! Apply recipes across every target partition. A recipe is desugared into its
//! primitive tree, and each statement and expression has one implementation here
//! and in [`crate::scope`], whatever surface operation it came from.

use crate::{Error, Images, Target, defaults::Defaults, package::Opened, scope::Scope};
use model::{
    DevicePath, EntryKind, Mounts, Name, PartitionTree, RelativePath, SourceRef, TargetPath,
};
use recipe::{
    Data, Firmware, Recipe, STANDARD_SYSTEM, VENDOR_SIDE,
    tree::{Expr, Stmt, Transform},
    value::{Place, Value},
};
use serde::Serialize;
use std::{collections::BTreeMap, path::Path};
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

/// A pattern a step removed that matched nothing.
#[derive(Clone, Debug, Serialize)]
pub struct Absent {
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
    absent: Vec<Absent>,
    /// Every `SetMetadata` statement, and whether it changed anything.
    set: Vec<(Absent, bool)>,
    /// `SetMetadata` on new entries, applied after their defaults.
    deferred: Vec<Deferred>,
    /// The package a `Within` has open.
    package: Option<Opened>,
}

/// A `SetMetadata` on an entry waiting for Android's defaults.
struct Deferred {
    statement: usize,
    partition: Name,
    path: RelativePath,
    overrides: recipe::Overrides,
    step: Name,
}

/// What a composition reports besides the trees.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    pub skipped: Vec<Skipped>,
    pub absent: Vec<Absent>,
    /// `SetMetadata` patterns that changed nothing: what they set is already there.
    pub unchanged: Vec<Absent>,
}

impl Composition {
    pub fn new(targets: BTreeMap<Name, Target>, origins: BTreeMap<Name, Firmware>) -> Self {
        Self {
            targets,
            origins,
            skipped: Vec::new(),
            absent: Vec::new(),
            set: Vec::new(),
            deferred: Vec::new(),
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
                Stmt::Copy { .. } | Stmt::Move { .. } | Stmt::SetMeta { .. } | Stmt::Within { .. }
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
            Stmt::When { condition, body } => {
                if self.eval(condition, context, step)?.as_bool()? {
                    for stmt in body {
                        self.exec(stmt, recipe, step, context)?;
                    }
                } else {
                    let reason = format!("condition does not hold: {}", ron(condition));
                    self.skip(recipe, step, reason);
                }
            }
            Stmt::Delete { selection, pattern } => {
                let selected = self.eval(selection, context, step)?;
                let selected = selected.as_selection()?;
                if selected.is_empty() {
                    self.absent.push(Absent {
                        step: step.clone(),
                        pattern: pattern.clone(),
                    });
                }
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
            } => {
                let selected = self.eval(selection, context, step)?;
                let selected = selected.as_selection()?;
                if selected.is_empty() {
                    return Err(Error::NothingSelected {
                        step: step.clone(),
                        pattern: ron(selection),
                    });
                }
                let to = to
                    .as_ref()
                    .map(|to| self.resolve_one(to, context, step))
                    .transpose()?;
                if to.is_some() && selected.len() > 1 {
                    return Err(Error::Unsupported(
                        "a copy of several entries keeps their paths",
                    ));
                }
                for entry in selected {
                    let Place::Firmware(firmware) = entry.place else {
                        return Err(Error::Unsupported(
                            "Copy reads a firmware; Move moves within the build",
                        ));
                    };
                    let source = crate::scope::source_of(firmware, &entry.partition)?;
                    let tree = context
                        .sources
                        .get(&source)
                        .ok_or_else(|| Error::UnknownSource(source.clone()))?;
                    let to = to.clone().unwrap_or_else(|| TargetPath {
                        partition: entry.partition.clone(),
                        path: entry.path.clone(),
                    });
                    self.target(&to.partition)?.import(
                        tree,
                        &source,
                        &entry.path,
                        &to.path,
                        *metadata,
                        step,
                    )?;
                }
            }
            Stmt::Move { from, to } => {
                let from = self.resolve_one(from, context, step)?;
                let to = self.resolve_one(to, context, step)?;
                // Moved entries keep their metadata; those still waiting for
                // Android's defaults wait at their new path.
                let below = |path: &RelativePath, root: &RelativePath| {
                    path == root || path.as_ref().starts_with(&format!("{root}/"))
                };
                let waiting: Vec<RelativePath> = self
                    .target(&from.partition)?
                    .created
                    .iter()
                    .filter(|path| below(path, &from.path))
                    .map(|path| {
                        RelativePath::try_from(format!(
                            "{}{}",
                            to.path,
                            &path.as_ref()[from.path.as_ref().len()..]
                        ))
                    })
                    .collect::<Result<_, _>>()?;
                let before: Vec<_> = [&from.partition, &to.partition]
                    .into_iter()
                    .map(|partition| Ok(self.target(partition)?.created.clone()))
                    .collect::<Result<_, Error>>()?;
                if from.partition == to.partition {
                    self.target(&to.partition)?
                        .relocate(&from.path, &to.path, step)?;
                } else {
                    let mut source = self
                        .targets
                        .remove(&from.partition)
                        .ok_or_else(|| Error::UnknownPartition(from.partition.clone()))?;
                    let moved = self
                        .target(&to.partition)
                        .and_then(|destination| source.relocate_to(destination, &from, &to, step));
                    self.targets.insert(from.partition.clone(), source);
                    moved?;
                }
                self.target(&from.partition)?.created = before[0]
                    .iter()
                    .filter(|path| !below(path, &from.path))
                    .cloned()
                    .collect();
                if from.partition != to.partition {
                    self.target(&to.partition)?.created = before[1].clone();
                }
                self.target(&to.partition)?.created.extend(waiting);
            }
            Stmt::Write { asset, to } => {
                let source = asset.resolve(context.assets);
                if let Some(package) = &self.package {
                    let path = RelativePath::try_from(to.to_string()[1..].to_owned())?
                        .resolve(&package.decoded);
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
            }
            Stmt::SetMeta {
                selection,
                pattern,
                overrides,
            } => {
                let selected = self.eval(selection, context, step)?;
                let selected = selected.as_selection()?;
                if selected.is_empty() {
                    return Err(Error::NothingSelected {
                        step: step.clone(),
                        pattern: pattern.clone(),
                    });
                }
                let statement = self.set.len();
                self.set.push((
                    Absent {
                        step: step.clone(),
                        pattern: pattern.clone(),
                    },
                    false,
                ));
                for entry in selected {
                    let target = self.target(&entry.partition)?;
                    // A new entry gets Android's defaults when the build is done,
                    // and this after them.
                    if target.created.contains(&entry.path) {
                        self.deferred.push(Deferred {
                            statement,
                            partition: entry.partition.clone(),
                            path: entry.path.clone(),
                            overrides: overrides.clone(),
                            step: step.clone(),
                        });
                    } else if target.set_metadata(&entry.path, overrides, step)? {
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
                if selected.is_empty() {
                    return Err(Error::NothingSelected {
                        step: step.clone(),
                        pattern: ron(selection),
                    });
                }
                let transform = &match transform {
                    Transform::Append(Data::File { from, path }) => {
                        Transform::Append(Data::Text(firmware_text(*from, path, context)?))
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
                    return Err(Error::Assertion(message.clone()));
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

    /// Give new entries Android's defaults, from the finished build's own policy as
    /// the device labels with it, then the `SetMetadata` that came after them; write
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
            .any(|target| !target.created.is_empty())
        {
            let defaults = Defaults::new(&self.targets, mounts, images)?;
            for (name, target) in &mut self.targets {
                target.give_defaults(name, mounts, &defaults)?;
            }
        }
        for deferred in std::mem::take(&mut self.deferred) {
            let target = self.target(&deferred.partition)?;
            if target.tree.entries.contains_key(&deferred.path)
                && target.set_metadata(&deferred.path, &deferred.overrides, &deferred.step)?
            {
                self.set[deferred.statement].1 = true;
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
                absent: self.absent,
                unchanged,
            },
        ))
    }
}

fn ron(value: &impl Serialize) -> String {
    ron::to_string(value).unwrap_or_default()
}

/// A firmware file's content, as text.
fn firmware_text(
    from: Firmware,
    path: &DevicePath,
    context: &Context<'_>,
) -> Result<String, Error> {
    let [at] = <[TargetPath; 1]>::try_from(context.mounts.resolve(path))
        .map_err(|_| Error::Unsupported("Append reads one firmware file"))?;
    let source = crate::scope::source_of(from, &at.partition)?;
    let tree = context
        .sources
        .get(&source)
        .ok_or_else(|| Error::UnknownSource(source.clone()))?;
    let mut entry = tree
        .entries
        .get(&at.path)
        .ok_or_else(|| Error::Missing(at.path.clone()))?;
    if let EntryKind::Hardlink { target } = &entry.kind {
        entry = tree
            .entries
            .get(target)
            .ok_or_else(|| Error::Missing(target.clone()))?;
    }
    let EntryKind::File { content, size, .. } = &entry.kind else {
        return Err(Error::Unsupported("Append reads a regular file"));
    };
    let store = android_fs::Store {
        images: context.images,
        root: Path::new("/"),
    };
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut store.open(content, *size)?, &mut bytes)?;
    String::from_utf8(bytes).map_err(|_| Error::Unsupported("appended files must be UTF-8"))
}
