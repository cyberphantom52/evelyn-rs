//! Apply recipes across every target partition: each step's `let:` and `when:`,
//! then its operations, each a function in [`crate::patch::operations`].

use crate::patch::{
    Error, Images, Target, defaults::Defaults, layout, package::Opened, target::Placing,
};
use crate::recipe::{Expr, Metadata, Operation, Recipe, value::Located};
use crate::toolchain::Tools;
use android::PartitionTree;
use android::firmware::{Firmware, SourceRef};
use android::partition::{Mounts, STANDARD_SYSTEM, TargetPath, VENDOR_SIDE};
use android::path::{DevicePath, Name, RelativePath};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// Scanned source trees, by source reference.
pub type Sources = BTreeMap<SourceRef, PartitionTree>;

/// What operations need beyond the targets themselves.
pub struct Context<'a> {
    pub sources: &'a Sources,
    /// The open source images that untouched file contents are read from.
    pub images: &'a Images,
    pub tools: &'a Tools,
    /// The recipe's directory, which asset paths resolve below; `None` for a
    /// recipe built into Evelyn, whose assets are files Evelyn downloads.
    pub assets: Option<&'a Path>,
    /// Scratch directory for decoded packages.
    pub packages: &'a Path,
    /// Where each partition is mounted, for resolving device paths.
    pub mounts: &'a Mounts,
}

impl Context<'_> {
    /// Where an asset is, downloading it first for a recipe built into Evelyn.
    pub(crate) fn asset(&self, path: &RelativePath) -> Result<PathBuf, Error> {
        match self.assets {
            Some(dir) => Ok(path.resolve(dir)),
            None => Ok(self.tools.file(path.as_ref())?),
        }
    }

    /// Whether an asset is there: for a recipe built into Evelyn, whether Evelyn
    /// has it pinned.
    pub(crate) fn has_asset(&self, path: &RelativePath) -> bool {
        match self.assets {
            Some(dir) => path.resolve(dir).exists(),
            None => self.tools.pinned(path.as_ref()),
        }
    }

    /// The scanned tree of a firmware's partition.
    pub(crate) fn source(&self, source: &SourceRef) -> Result<&PartitionTree, Error> {
        self.sources
            .get(source)
            .ok_or_else(|| Error::UnknownSource(source.clone()))
    }
}

/// A step, or part of one, that did not apply, and why.
#[derive(Clone, Debug, Serialize)]
pub struct Skipped {
    pub recipe: Name,
    pub step: Name,
    pub reason: String,
}

pub struct Composition {
    pub(crate) targets: BTreeMap<Name, Target>,
    /// Which firmware each target partition was built from.
    pub(crate) origins: BTreeMap<Name, Firmware>,
    skipped: Vec<Skipped>,
    /// What the step being applied reported skipping.
    pub(crate) skips: Vec<String>,
    /// The package a `Package` has open.
    pub(crate) package: Option<Opened>,
}

/// The values the strings of an operation can name as `{name}`: a step's `let:`,
/// a kit operation's parameters, and what a `Filter` or `ForEach` binds.
pub(crate) type Bound = BTreeMap<Name, String>;

impl Composition {
    pub fn new(targets: BTreeMap<Name, Target>, origins: BTreeMap<Name, Firmware>) -> Self {
        Self {
            targets,
            origins,
            skipped: Vec::new(),
            skips: Vec::new(),
            package: None,
        }
    }

    /// Apply a recipe, its steps in dependency order. An error says which step
    /// it came from.
    pub fn apply(&mut self, recipe: &Recipe, context: &Context<'_>) -> Result<(), Error> {
        for step in recipe.ordered_steps()? {
            let bindings = (&step.bindings, step.when.as_ref());
            self.body(bindings, &step.operations, Bound::new(), context)
                .map_err(|source| Error::Step {
                    step: step.id.clone(),
                    source: Box::new(source),
                })?;
            for reason in self.skips.drain(..) {
                self.skipped.push(Skipped {
                    recipe: recipe.name.clone(),
                    step: step.id.clone(),
                    reason,
                });
            }
        }
        Ok(())
    }

    /// Bind each `let:` name to its value, then, if `when:` holds, apply the
    /// operations. A value that is not set, or a condition that does not hold,
    /// skips them, and is reported. A bound string is a token, `[A-Za-z0-9._-]`:
    /// it comes from firmware and goes into paths.
    pub(crate) fn body(
        &mut self,
        (bindings, when): (&BTreeMap<Name, Expr>, Option<&Expr>),
        operations: &[Operation],
        mut bound: Bound,
        context: &Context<'_>,
    ) -> Result<(), Error> {
        for (name, value) in bindings {
            let Some(text) = self
                .eval(value, &bound, context)?
                .as_str()?
                .map(str::to_owned)
            else {
                self.skip(format!("{name} is not set: {value}"));
                return Ok(());
            };
            if !android::firmware::is_token(&text) {
                return Err(crate::recipe::Error::Invalid(format!(
                    "{name} is bound to {text:?}, which is not [A-Za-z0-9._-]"
                ))
                .into());
            }
            bound.insert(name.clone(), text);
        }
        if let Some(condition) = when
            && !self.eval(condition, &bound, context)?.as_bool()?
        {
            self.skip(format!("condition does not hold: {condition}"));
            return Ok(());
        }
        operations
            .iter()
            .try_for_each(|operation| self.run(operation, &bound, context))
    }

    /// Report that part of the step being applied did not apply, and why.
    pub(crate) fn skip(&mut self, reason: String) {
        self.skips.push(reason);
    }

    /// Copy what a firmware has at `entry` into the build.
    pub(crate) fn copy_firmware(
        &mut self,
        firmware: Firmware,
        entry: &Located,
        to: Option<&DevicePath>,
        placing: Placing,
        context: &Context<'_>,
    ) -> Result<(), Error> {
        let source = crate::patch::files::source_of(firmware, &entry.partition);
        let tree = context.source(&source)?;
        let to = match to {
            Some(to) => self.resolve_one(to, context)?,
            None => TargetPath {
                partition: entry.partition.clone(),
                path: entry.path.clone(),
            },
        };
        self.target(&to.partition)?
            .import(tree, &entry.path, &to.path, placing)
    }

    /// Copy a recipe asset (a file, or a directory's files) to `to`, in the build
    /// or the open package. An asset has no metadata of its own.
    pub(crate) fn copy_asset(
        &mut self,
        asset: &RelativePath,
        to: &DevicePath,
        placing: Placing,
        context: &Context<'_>,
    ) -> Result<(), Error> {
        let source = context.asset(asset)?;
        if let Some(package) = &self.package {
            let inside = RelativePath::try_from(to.to_string()[1..].to_owned())?;
            let path = inside.resolve(&package.decoded);
            if path.exists() && !placing.overwrite {
                return Err(layout::Error::DestinationExists(inside).into());
            }
            if let Some(parent) = path.parent() {
                if !parent.exists() && !placing.create_parent {
                    return Err(layout::Error::MissingParent(inside).into());
                }
                std::fs::create_dir_all(parent).map_err(crate::patch::package::io(parent))?;
            }
            std::fs::copy(&source, &path).map_err(crate::patch::package::io(&source))?;
            return Ok(());
        }
        // A partition's mount point is its root; anything else is a path in one.
        let (partition, base) = match context.mounts.partition_at(to) {
            Some(partition) => (partition.clone(), None),
            None => {
                let at = self.resolve_one(to, context)?;
                (at.partition, Some(at.path))
            }
        };
        let below = |path: &str| match &base {
            Some(base) => RelativePath::try_from(format!("{base}/{path}")),
            None => RelativePath::try_from(path.to_owned()),
        };
        let files = if source.is_dir() {
            files(&source)?
                .into_iter()
                .map(|(relative, file)| Ok((below(&relative)?, file)))
                .collect::<Result<Vec<_>, Error>>()?
        } else {
            let to = base.ok_or(Error::Unsupported(
                "a file cannot replace a partition's root",
            ))?;
            vec![(to, source)]
        };
        if placing.metadata == Metadata::Source {
            return Err(Error::Unsupported(
                "an asset has no metadata of its own to keep",
            ));
        }
        let target = self.target(&partition)?;
        for (to, file) in files {
            target.install(&file, &to, placing)?;
        }
        Ok(())
    }

    /// Copy an entry of the build, and what is below it, to `to`, keeping their
    /// metadata; those still waiting for Android's defaults wait at the copy too.
    pub(crate) fn copy_build(
        &mut self,
        entry: &Located,
        to: &DevicePath,
        placing: Placing,
        context: &Context<'_>,
    ) -> Result<(), Error> {
        let to = self.resolve_one(to, context)?;
        if entry.partition == to.partition {
            self.target(&to.partition)?
                .copy_within(&entry.path, &to.path, placing)
        } else {
            let source = self
                .targets
                .remove(&entry.partition)
                .ok_or_else(|| Error::UnknownPartition(entry.partition.clone()))?;
            let copied = self.target(&to.partition).and_then(|destination| {
                source.copy_into(destination, &entry.path, &to.path, placing)
            });
            self.targets.insert(entry.partition.clone(), source);
            copied
        }
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
    pub(crate) fn resolve_one(
        &self,
        path: &DevicePath,
        context: &Context<'_>,
    ) -> Result<TargetPath, Error> {
        let mut found = Self::resolve(&self.targets, context.mounts, path);
        match found.len() {
            1 => Ok(found.remove(0)),
            0 => Err(Error::AbsentPath(path.clone())),
            _ => Err(Error::Unsupported(
                "a path to edit must name one partition, not a glob over several",
            )),
        }
    }

    pub(crate) fn target(&mut self, name: &Name) -> Result<&mut Target, Error> {
        self.targets
            .get_mut(name)
            .ok_or_else(|| Error::UnknownPartition(name.clone()))
    }

    /// Give the entries waiting for them Android's defaults, from the finished
    /// build's own policy as the device labels with it, then what was set over
    /// them; the finished trees, with the steps that were skipped.
    pub fn finish(
        self,
        mounts: &Mounts,
        images: &Images,
    ) -> Result<(BTreeMap<Name, PartitionTree>, Vec<Skipped>), Error> {
        let defaults = match self.targets.values().any(Target::waits_for_defaults) {
            true => Some(Defaults::new(&self.targets, mounts, images)?),
            false => None,
        };
        let trees = self
            .targets
            .into_iter()
            .map(|(name, target)| {
                let tree = target.resolve(&name, mounts, defaults.as_ref())?;
                Ok((name, tree))
            })
            .collect::<Result<_, Error>>()?;
        Ok((trees, self.skipped))
    }
}

/// The regular files under `root`, as (path relative to `root`, full path), sorted.
/// Anything other than files and directories is an error.
fn files(root: &Path) -> std::io::Result<Vec<(String, PathBuf)>> {
    let mut found = Vec::new();
    let mut pending = vec![(String::new(), root.to_path_buf())];
    while let Some((prefix, dir)) = pending.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push((relative, entry.path()));
            } else if kind.is_file() {
                found.push((relative, entry.path()));
            } else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "{} is neither a file nor a directory",
                        entry.path().display()
                    ),
                ));
            }
        }
    }
    found.sort();
    Ok(found)
}
