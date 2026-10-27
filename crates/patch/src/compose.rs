//! Apply recipes across every target partition. A recipe is desugared into its
//! primitive tree, and each statement and expression has one implementation here
//! and in [`crate::scope`], whatever surface operation it came from.

use crate::{Error, Images, Target, scope::Scope};
use model::{DevicePath, Mounts, Name, PartitionTree, SourceRef, TargetPath};
use recipe::{
    Firmware, Recipe, STANDARD_SYSTEM, VENDOR_SIDE,
    tree::{Expr, Stmt},
    value::{Place, Value},
};
use serde::Serialize;
use std::{collections::BTreeMap, path::Path};

/// Scanned source trees, by source reference.
pub type Sources = BTreeMap<SourceRef, PartitionTree>;

/// What operations need beyond the targets themselves.
pub struct Context<'a> {
    pub sources: &'a Sources,
    /// The open source images that untouched file contents are read from.
    pub images: &'a Images,
    /// The recipe's directory; asset paths resolve below it.
    pub assets: &'a Path,
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
}

/// What a composition reports besides the trees.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    pub skipped: Vec<Skipped>,
    pub absent: Vec<Absent>,
}

impl Composition {
    pub fn new(targets: BTreeMap<Name, Target>, origins: BTreeMap<Name, Firmware>) -> Self {
        Self {
            targets,
            origins,
            skipped: Vec::new(),
            absent: Vec::new(),
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
                    match entry.place {
                        Place::Build => {
                            self.target(&entry.partition)?.remove(&entry.path, step)?;
                        }
                        _ => return Err(Error::Unsupported("only the build can be removed from")),
                    }
                }
            }
            Stmt::Copy {
                selection,
                to,
                overrides,
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
                        overrides,
                        step,
                    )?;
                }
            }
            Stmt::Move { from, to } => {
                let from = self.resolve_one(from, context, step)?;
                let to = self.resolve_one(to, context, step)?;
                if from.partition == to.partition {
                    return self
                        .target(&to.partition)?
                        .relocate(&from.path, &to.path, step);
                }
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
            Stmt::Write {
                asset,
                sha256,
                to,
                metadata,
            } => {
                let at = self.resolve_one(to, context, step)?;
                let target = self.target(&at.partition)?;
                let changed = target.install(
                    &asset.resolve(context.assets),
                    asset,
                    sha256,
                    &at.path,
                    metadata,
                )?;
                target.accept(changed, step);
            }
            Stmt::WriteTree {
                assets,
                partition,
                rules,
            } => {
                if !self.targets.contains_key(partition) {
                    return Err(Error::AbsentPartition {
                        step: step.clone(),
                        partition: partition.clone(),
                    });
                }
                self.target(partition)?
                    .install_tree(context.assets, assets, rules, step)?;
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
                for entry in selected {
                    match entry.place {
                        Place::Build => self.target(&entry.partition)?.transform(
                            &entry.path,
                            transform,
                            context,
                            step,
                        )?,
                        _ => return Err(Error::Unsupported("only the build can be edited")),
                    }
                }
            }
            Stmt::Assert { condition, message } => {
                if !self.eval(condition, context, step)?.as_bool()? {
                    return Err(Error::Assertion(message.clone()));
                }
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

    /// Write every target's manifest and return the finished trees, with the steps
    /// that were skipped and the removed patterns that matched nothing.
    pub fn finish(self) -> Result<(BTreeMap<Name, PartitionTree>, Report), Error> {
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
            },
        ))
    }
}

fn ron(value: &impl Serialize) -> String {
    ron::to_string(value).unwrap_or_default()
}
