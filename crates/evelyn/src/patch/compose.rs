//! Apply recipes across every target partition: each step's operations in
//! order, each a function in [`crate::patch::operations`].

use crate::patch::target::IoAt;
use crate::patch::value::Located;
use crate::patch::{
    Error, Images, Target,
    defaults::Defaults,
    layout,
    package::Package,
    target::{Open, Placing},
};
use crate::recipe::{Metadata, Recipe};
use crate::toolchain::Tools;
use crate::{Firmware, SourceRef};
use android::PartitionTree;
use android::partition::{Mounts, PartitionPath, STANDARD_SYSTEM, VENDOR_SIDE};
use android::path::{DevicePath, RelativePath};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
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
    /// Where each partition is mounted, for resolving device paths.
    pub mounts: &'a Mounts,
    /// Each stock boot image's place among `images`.
    pub boot_images: &'a BTreeMap<String, usize>,
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
    /// The recipe's path.
    pub recipe: PathBuf,
    pub step: String,
    pub reason: String,
}

pub struct Composition {
    pub(crate) targets: BTreeMap<String, Target>,
    /// Which firmware each target partition was built from.
    pub(crate) origins: BTreeMap<String, Firmware>,
    skipped: Vec<Skipped>,
    /// What the step being applied reported skipping.
    pub(crate) skips: Vec<String>,
    /// Where the package a `Package` is applying its edits to is.
    pub(crate) package: Option<PartitionPath>,
    /// The boot image a `BootImage` is applying its edits to.
    pub(crate) boot_image: Option<String>,
    /// The boot images whose parts other than the ramdisk a `BootImage` edits,
    /// opened from the stock ones and edited in place; each is built with its
    /// ramdisk when the build is written.
    pub(crate) boot_documents: BTreeMap<String, bootimg::Image>,
    /// What the step being applied left out of the policies it imported.
    pub(crate) held: Vec<Held>,
}

/// The rules a step left out of its imports, and the names they need.
type LeftOut = (BTreeSet<String>, BTreeSet<String>);

/// A rule a policy `Import` left out of the policy file `file`, and the names
/// it needs that nothing defined then, until its step is done and the file
/// holds it.
#[derive(Clone, Debug)]
pub(crate) struct Held {
    pub(crate) file: Located,
    pub(crate) rule: String,
    pub(crate) needs: BTreeSet<String>,
}

/// The values the strings of an operation can name as `{name}`: what a `Let` binds,
/// a kit operation's parameters, and what a `Filter` or `ForEach` binds.
pub(crate) type Bound = BTreeMap<String, String>;

impl Composition {
    pub fn new(targets: BTreeMap<String, Target>, origins: BTreeMap<String, Firmware>) -> Self {
        Self {
            targets,
            origins,
            skipped: Vec::new(),
            skips: Vec::new(),
            package: None,
            boot_image: None,
            boot_documents: BTreeMap::new(),
            held: Vec::new(),
        }
    }

    /// Apply the recipe at `path`, its steps in order. An error says which step
    /// it came from.
    pub fn apply(
        &mut self,
        recipe: &Recipe,
        path: &Path,
        context: &Context<'_>,
    ) -> Result<(), Error> {
        for step in &recipe.steps {
            self.operations(step.operations(), Bound::new(), context)
                .map_err(|source| Error::Step {
                    step: step.id().to_owned(),
                    source: Box::new(source),
                })?;
            for reason in self.skips.drain(..) {
                self.skipped.push(Skipped {
                    recipe: path.to_path_buf(),
                    step: step.id().to_owned(),
                    reason,
                });
            }
            for held in std::mem::take(&mut self.held) {
                let rule = crate::patch::sepolicy::Held {
                    recipe: path.to_path_buf(),
                    step: step.id().to_owned(),
                    rule: held.rule,
                    needs: held.needs,
                };
                self.document(&held.file)
                    .map_err(|source| Error::Step {
                        step: step.id().to_owned(),
                        source: Box::new(source),
                    })?
                    .hold(rule);
            }
        }
        Ok(())
    }

    /// The package a `Package` is applying its edits to.
    pub(crate) fn current(&self) -> Option<&Package> {
        let at = self.package.as_ref()?;
        match self.targets.get(&at.partition)?.opened_ref(&at.path).ok()? {
            Some(Open::Package(package)) => Some(package),
            _ => None,
        }
    }

    /// The package a `Package` is applying its edits to, to change.
    pub(crate) fn current_mut(&mut self) -> Option<&mut Package> {
        let at = self.package.clone()?;
        match self.targets.get_mut(&at.partition)?.opened(&at.path).ok()? {
            Some(Open::Package(package)) => Some(package),
            _ => None,
        }
    }

    /// The boot image a `BootImage` is applying its edits to, opened from the
    /// stock one the first time.
    pub(crate) fn boot_document(
        &mut self,
        context: &Context<'_>,
    ) -> Result<&mut bootimg::Image, Error> {
        let name = self.boot_image.clone().ok_or(Error::Unsupported(
            "a boot image's parts are edited inside a BootImage",
        ))?;
        Ok(self.boot_documents.entry(name).or_insert_with_key(|name| {
            context.images[context.boot_images[name]]
                .boot_image()
                .expect("a BootImage names a boot image")
                .clone()
        }))
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
            None => PartitionPath {
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
        if let Some(package) = self.current_mut() {
            let inside = RelativePath::try_from(to.to_string()[1..].to_owned())?;
            let files = package.files()?;
            let path: &str = inside.as_ref();
            if files.exists(path) && !placing.overwrite {
                return Err(layout::Error::DestinationExists(inside).into());
            }
            let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
            if !parent.is_empty() && !files.is_dir(parent) && !placing.create_parent {
                return Err(layout::Error::MissingParent(inside).into());
            }
            let data = std::fs::read(&source).map_err(crate::patch::package::io(&source))?;
            return Ok(package.write(path, data)?);
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
            files(&source)
                .at(&source)?
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
            let mut source = self
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
        targets: &BTreeMap<String, Target>,
        mounts: &Mounts,
        path: &DevicePath,
    ) -> Vec<PartitionPath> {
        let first = path.components().next().unwrap_or_default();
        let standard = VENDOR_SIDE.contains(&first) || STANDARD_SYSTEM.contains(&first);
        if standard && first != "system" && !targets.keys().any(|name| name.as_str() == first) {
            return Vec::new();
        }
        mounts.resolve(path)
    }

    /// The one place a device path is in the build.
    pub(crate) fn resolve_one(
        &self,
        path: &DevicePath,
        context: &Context<'_>,
    ) -> Result<PartitionPath, Error> {
        let mut found = Self::resolve(&self.targets, context.mounts, path);
        match found.len() {
            1 => Ok(found.remove(0)),
            0 => Err(Error::AbsentPath(path.clone())),
            _ => Err(Error::Unsupported(
                "a path to edit must name one partition, not a glob over several",
            )),
        }
    }

    pub(crate) fn target(&mut self, name: &str) -> Result<&mut Target, Error> {
        self.targets
            .get_mut(name)
            .ok_or_else(|| Error::UnknownPartition(name.to_owned()))
    }

    /// Write every open document back into its file, and report the rules an
    /// import left out that still need a name no policy defines, with their step
    /// and the names they need.
    pub fn write_back(&mut self) -> Result<(), Error> {
        // For each step, the rules it left out that still need a name no
        // policy defines, and those names, before the policies are written.
        let mut left: BTreeMap<(PathBuf, String), LeftOut> = BTreeMap::new();
        for target in self.targets.values() {
            for (_, open) in target.open_files() {
                let Open::Policy(document) = open else {
                    continue;
                };
                for held in document.unresolved() {
                    tracing::debug!(step = %held.step, needs = ?held.needs, rule = %held.rule, "policy rule left out");
                    let (rules, names) = left
                        .entry((held.recipe.clone(), held.step.clone()))
                        .or_default();
                    rules.insert(held.rule.clone());
                    names.extend(held.needs.iter().cloned());
                }
            }
        }
        for target in self.targets.values_mut() {
            target.close(None)?;
        }
        for ((recipe, step), (rules, names)) in left {
            let plural = if rules.len() == 1 { "rule" } else { "rules" };
            let names: Vec<String> = names.into_iter().collect();
            self.skipped.push(Skipped {
                recipe,
                step,
                reason: format!(
                    "{} policy {plural} left out, needing what no policy defines: {}",
                    rules.len(),
                    names.join(", ")
                ),
            });
        }
        Ok(())
    }

    /// Give the entries waiting for them Android's defaults, from the finished
    /// build's own policy as the device labels with it, then what was set over
    /// them; the finished trees, with the steps that were skipped. The documents
    /// are written back first.
    pub fn finish(
        mut self,
        mounts: &Mounts,
        images: &Images,
    ) -> Result<(BTreeMap<String, PartitionTree>, Vec<Skipped>), Error> {
        self.write_back()?;
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

/// Whether `text` is a non-empty run of `[A-Za-z0-9._-]`: safe in a path component
/// or a recipe, for a value read from firmware.
pub(crate) fn is_token(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
