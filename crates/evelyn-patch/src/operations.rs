use crate::{Error, Target, compose::Context, layout};
use android_firmware::{AndroidMetadata, Name, RelativePath, Sha256Digest, SourceRef};
use android_fs::{Entry, EntryKind, PartitionTree, Provenance};
use recipe::{
    Returns,
    tree::{Expr, Metadata, MethodEdit, Overrides, Transform},
    value::Value,
};
use std::{collections::BTreeSet, path::Path};

impl Target {
    /// Remove `path` (or every entry a glob matches) and its contents. Returns
    /// whether there was anything to remove.
    pub(crate) fn remove(&mut self, path: &RelativePath, step: &Name) -> Result<bool, Error> {
        let matches = layout::matches(&self.tree, path);
        if matches.is_empty() {
            return Ok(false);
        }
        let mut changed = self.tree.clone();
        for path in matches {
            // A match may already be gone with a directory matched before it.
            if changed.entries.contains_key(&path) {
                changed = layout::remove(&changed, &path)?;
            }
        }
        self.accept(changed, step);
        Ok(true)
    }

    /// Copy `path` from a firmware's `source` tree; the paths written. With the
    /// source's metadata, a directory made for it must be one the source has,
    /// whose metadata it takes.
    pub(crate) fn import(
        &mut self,
        source: &PartitionTree,
        from: &SourceRef,
        path: &RelativePath,
        to_path: &RelativePath,
        placing: Placing,
        step: &Name,
    ) -> Result<BTreeSet<RelativePath>, Error> {
        // A wildcard import keeps every match at its own path.
        let pairs = if layout::is_glob(path) {
            if to_path != path {
                return Err(Error::Unsupported("wildcard imports must keep their path"));
            }
            let matches = layout::matches(source, path);
            if matches.is_empty() {
                return Err(layout::Error::MissingPath(path.clone()).into());
            }
            matches.into_iter().map(|m| (m.clone(), m)).collect()
        } else {
            vec![(path.clone(), to_path.clone())]
        };
        let mut written = BTreeSet::new();
        for (from_path, to_path) in pairs {
            let selected = layout::select(source, &from_path, &to_path, from)?;
            let mut changed = self.destination(&to_path, placing, Some(source))?;
            if placing.metadata == Metadata::Source
                && let Some(made) = changed.entries.keys().find(|path| {
                    !self.tree.entries.contains_key(*path)
                        && !selected.contains_key(*path)
                        && !source.entries.contains_key(*path)
                })
            {
                return Err(layout::Error::MissingPath(made.clone()).into());
            }
            changed.entries.extend(self.adopt(None, selected)?);
            written.extend(self.accept(changed, step));
        }
        Ok(written)
    }

    /// Write `file` at `to`. This replaces the directory entry, not every alias of
    /// its old inode. The file has no metadata of its own: the copy gives it some.
    pub(crate) fn install(
        &mut self,
        file: &Path,
        asset: &RelativePath,
        to: &RelativePath,
        placing: Placing,
    ) -> Result<PartitionTree, Error> {
        let previous = self.tree.entries.get(to);
        if previous.is_some_and(|entry| matches!(entry.kind, EntryKind::Directory)) {
            return Err(Error::Unsupported(
                "remove a directory explicitly before installing a file",
            ));
        }
        let (metadata, mut lineage) = match previous {
            Some(entry) => (entry.metadata.clone(), entry.provenance.clone()),
            None => (self.tree.root.clone(), Vec::new()),
        };
        let mut changed = self.destination(to, placing, None)?;
        let bytes = std::fs::read(file)?;
        lineage.push(Provenance::Asset {
            path: asset.clone(),
            sha256: Sha256Digest::bytes(&bytes),
        });
        changed.entries.insert(
            to.clone(),
            Entry {
                kind: self.write_blob(bytes.as_slice())?,
                metadata,
                provenance: lineage,
            },
        );
        Ok(changed)
    }

    /// Set what `overrides` sets on the entry at `path`, and whether that changed
    /// it; for an entry waiting for Android's defaults, `None`: it is set over them.
    pub(crate) fn set_metadata(
        &mut self,
        path: &RelativePath,
        overrides: &Overrides,
        statement: usize,
        step: &Name,
    ) -> Result<Option<bool>, Error> {
        if let Some(pending) = self.derived.get_mut(path) {
            pending.push(Pending {
                statement,
                overrides: overrides.clone(),
                step: step.clone(),
            });
            return Ok(None);
        }
        let entry = self
            .tree
            .entries
            .get_mut(path)
            .ok_or_else(|| Error::Missing(path.clone()))?;
        if !set(&mut entry.metadata, overrides) {
            return Ok(Some(false));
        }
        entry.provenance.push(Provenance::Step { id: step.clone() });
        Ok(Some(true))
    }

    /// Have `path` get Android's defaults; what was set on it before no longer
    /// applies.
    pub(crate) fn reset(&mut self, path: &RelativePath) {
        if self.tree.entries.contains_key(path) {
            self.derived.insert(path.clone(), Vec::new());
        }
    }

    /// Give every entry waiting for them Android's defaults for where it is on the
    /// device, then what was set over them. Returns the `SetMetadata` statements that
    /// changed something.
    pub(crate) fn give_defaults(
        &mut self,
        partition: &Name,
        mounts: &android_firmware::Mounts,
        defaults: &crate::defaults::Defaults,
    ) -> Result<Vec<usize>, Error> {
        let mut changed = Vec::new();
        for (path, pending) in std::mem::take(&mut self.derived) {
            let Some(entry) = self.tree.entries.get_mut(&path) else {
                continue;
            };
            let at = android_firmware::TargetPath {
                partition: partition.clone(),
                path: path.clone(),
            };
            let device = mounts
                .device_path(&at)
                .ok_or_else(|| Error::UnknownPartition(partition.clone()))?;
            entry.metadata = defaults.metadata(&device, &entry.kind)?;
            for pending in pending {
                if set(&mut entry.metadata, &pending.overrides) {
                    changed.push(pending.statement);
                    entry.provenance.push(Provenance::Step { id: pending.step });
                }
            }
        }
        Ok(changed)
    }

    /// Transform a file's content for its whole hardlink group, keeping metadata.
    pub(crate) fn transform(
        &mut self,
        path: &RelativePath,
        transform: &Transform,
        context: &Context<'_>,
        step: &Name,
    ) -> Result<(), Error> {
        let canonical = self.file(path)?.path.clone();
        let bytes = self.read(path, context.images)?;
        let changed = transformed(&bytes, transform, context)?;
        if changed == bytes {
            return Ok(());
        }
        let kind = self.write_blob(changed.as_slice())?;
        self.accept_content(&canonical, kind, step);
        Ok(())
    }

    /// The tree with `path` cleared for a new entry: an entry there is removed
    /// only to `overwrite` it, and missing parent directories are made, like
    /// `like`'s, only to `create_parent`.
    pub(crate) fn destination(
        &self,
        path: &RelativePath,
        placing: Placing,
        like: Option<&PartitionTree>,
    ) -> Result<PartitionTree, Error> {
        let mut changed = if self.tree.entries.contains_key(path) {
            if !placing.overwrite {
                return Err(layout::Error::DestinationExists(path.clone()).into());
            }
            layout::remove(&self.tree, path)?
        } else {
            self.tree.clone()
        };
        if !placing.create_parent
            && let Some((parent, _)) = path.as_ref().rsplit_once('/')
        {
            let parent = RelativePath::try_from(parent.to_owned())?;
            if !changed.entries.contains_key(&parent) {
                return Err(layout::Error::MissingParent(parent).into());
            }
        }
        layout::create_parents(&mut changed, path, like)?;
        Ok(changed)
    }

    /// Adopt `changed`, recording the step on every new or altered entry; those
    /// paths.
    pub(crate) fn accept(
        &mut self,
        mut changed: PartitionTree,
        step: &Name,
    ) -> BTreeSet<RelativePath> {
        let mut written = BTreeSet::new();
        for (path, entry) in &mut changed.entries {
            if !self.tree.entries.get(path).is_some_and(|previous| {
                previous.kind == entry.kind
                    && previous.metadata == entry.metadata
                    && previous.provenance == entry.provenance
            }) {
                entry.provenance.push(Provenance::Step { id: step.clone() });
                written.insert(path.clone());
            }
        }
        self.tree = changed;
        written
    }

    /// Give what a copy wrote the metadata it states: what the entries came with,
    /// and for those still waiting for Android's defaults where they came from,
    /// `waiting`; or Android's defaults.
    pub(crate) fn copied(
        &mut self,
        written: BTreeSet<RelativePath>,
        metadata: Metadata,
        waiting: Vec<(RelativePath, Vec<Pending>)>,
    ) {
        for path in written {
            match metadata {
                Metadata::Source => {
                    self.derived.remove(&path);
                }
                Metadata::Default => {
                    self.derived.insert(path, Vec::new());
                }
            }
        }
        if metadata == Metadata::Source {
            self.derived.extend(waiting);
        }
    }
}

/// How a copy writes: the metadata it gives, and whether it may write in place
/// of an entry and make missing parent directories.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Placing {
    pub metadata: Metadata,
    pub overwrite: bool,
    pub create_parent: bool,
}

/// A file's content after `transform`, wherever the file is: in the build or in a
/// package.
pub(crate) fn transformed(
    bytes: &[u8],
    transform: &Transform,
    context: &Context<'_>,
) -> Result<Vec<u8>, Error> {
    let text = |bytes: &[u8]| -> Result<String, Error> {
        String::from_utf8(bytes.to_vec())
            .map_err(|_| Error::Unsupported("text edits require UTF-8"))
    };
    // Its strings are evaluated before the edit, where expressions are.

    let asset_path =
        |expr: &Expr| Ok::<_, Error>(RelativePath::try_from(literal(expr)?.to_owned())?);
    let asset = |expr: &Expr| text(&std::fs::read(asset_path(expr)?.resolve(context.assets))?);
    Ok(match transform {
        Transform::WriteBytes(edits) => crate::binary::apply(bytes, edits)?,
        Transform::Xml(edits) => crate::xml::apply(bytes, edits)?,
        Transform::ApplyDiff(patch) => {
            crate::text::apply(&text(bytes)?, &asset(patch)?)?.into_bytes()
        }
        Transform::Property {
            key,
            value: Some(value),
            create,
        } => android_property::set(&text(bytes)?, key, value, *create).into_bytes(),
        Transform::Property {
            key, value: None, ..
        } => android_property::remove(&text(bytes)?, key).into_bytes(),
        Transform::Write(text) => literal(text)?.as_bytes().to_vec(),
        Transform::Smali {
            class,
            method,
            edit,
        } => {
            let source = text(bytes)?;
            match edit {
                MethodEdit::Stub(returns) => {
                    let stub = match returns {
                        Returns::Void => smali::Stub::Void,
                        Returns::Zero => smali::Stub::Zero,
                        Returns::One => smali::Stub::One,
                    };
                    smali::stub_method(&source, class, method, stub)?
                }
                MethodEdit::Replace(body) => {
                    smali::replace_method(&source, class, method, &asset(body)?)?
                }
                MethodEdit::Insert(body) => {
                    smali::insert_method(&source, class, method, &asset(body)?)?
                }
            }
            .into_bytes()
        }
        Transform::AddPolicy {
            policies,
            resolve_with: Expr::Literal(Value::List(resolve_with)),
        } => {
            let policies = policies
                .iter()
                .map(asset_path)
                .collect::<Result<Vec<_>, _>>()?;
            crate::sepolicy::apply(bytes, &policies, resolve_with, context)?
        }
        Transform::AddPolicy { .. } => {
            return Err(Error::Unsupported(
                "AddPolicy's policy files are evaluated before the edit",
            ));
        }
    })
}

/// A `SetMetadata` on an entry waiting for Android's defaults, set over them.
#[derive(Clone, Debug)]
pub(crate) struct Pending {
    pub statement: usize,
    pub overrides: Overrides,
    pub step: Name,
}

/// Set what `overrides` sets on `metadata`; whether that changed it.
fn set(metadata: &mut AndroidMetadata, overrides: &Overrides) -> bool {
    let before = metadata.clone();
    metadata.uid = overrides.uid.unwrap_or(metadata.uid);
    metadata.gid = overrides.gid.unwrap_or(metadata.gid);
    metadata.mode = overrides.mode.unwrap_or(metadata.mode);
    if let Some(label) = &overrides.selinux {
        metadata.set_selinux(label);
    }
    *metadata != before
}

/// A transform's string, evaluated before the edit, where expressions are.
fn literal(expr: &Expr) -> Result<&str, Error> {
    expr.literal_str().ok_or(Error::Unsupported(
        "a transform's strings are evaluated first",
    ))
}
