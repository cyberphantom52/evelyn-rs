use crate::{Error, Target, compose::Context, layout};
use android_firmware::{AndroidMetadata, Name, RelativePath, Sha256Digest, SourceRef};
use android_fs::{Entry, EntryKind, PartitionTree, Provenance};
use recipe::{
    Overrides, Returns,
    tree::{Expr, MethodEdit, Transform},
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

    /// Copy `path` from a firmware's `source` tree: overwritten entries keep this
    /// target's metadata, new ones have the source's.
    pub(crate) fn import(
        &mut self,
        source: &PartitionTree,
        from: &SourceRef,
        path: &RelativePath,
        to_path: &RelativePath,
        step: &Name,
    ) -> Result<(), Error> {
        // A wildcard import keeps every match at its own path.
        let pairs = if layout::is_glob(path) {
            if to_path != path {
                return Err(Error::Unsupported("wildcard imports must keep their path"));
            }
            let matches = layout::expand(source, path);
            if matches.is_empty() {
                return Err(layout::Error::MissingPath(path.clone()).into());
            }
            matches.into_iter().map(|m| (m.clone(), m)).collect()
        } else {
            vec![(path.clone(), to_path.clone())]
        };
        for (from_path, to_path) in pairs {
            let selected = layout::select(source, &from_path, &to_path, from)?;
            let mut changed = self.destination(&to_path, true, Some(source))?;
            changed.entries.extend(self.adopt(None, selected.clone())?);
            for (path, entry) in changed.entries.iter_mut() {
                let previous = self.tree.entries.get(path);
                let brought = selected.contains_key(path);
                if !brought && previous.is_some() {
                    continue;
                }
                if let Some(previous) = previous {
                    entry.metadata = previous.metadata.clone();
                }
            }
            let earlier: BTreeSet<RelativePath> = self.derived.keys().cloned().collect();
            self.accept(changed, step);
            // What the source has keeps the source's metadata: what it brought, and
            // the parent directories made like the source's.
            self.derived.retain(|path, _| {
                earlier.contains(path)
                    || !selected.contains_key(path)
                        && !source
                            .entries
                            .get(path)
                            .is_some_and(|entry| matches!(entry.kind, EntryKind::Directory))
            });
        }
        Ok(())
    }

    /// Write `file` at `to`. This replaces the directory entry, not every alias of
    /// its old inode. An overwritten file keeps its metadata; a new one is
    /// given Android's defaults once accepted.
    pub(crate) fn install(
        &mut self,
        file: &Path,
        asset: &RelativePath,
        to: &RelativePath,
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
        let mut changed = self.destination(to, true, None)?;
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

    /// The tree with `path` cleared for a new entry: its parent must be a
    /// directory, and an existing entry is removed only when `replace` is set.
    pub(crate) fn destination(
        &self,
        path: &RelativePath,
        replace: bool,
        like: Option<&PartitionTree>,
    ) -> Result<PartitionTree, Error> {
        let mut changed = if self.tree.entries.contains_key(path) {
            if !replace {
                return Err(layout::Error::DestinationExists(path.clone()).into());
            }
            layout::remove(&self.tree, path)?
        } else {
            self.tree.clone()
        };
        layout::create_parents(&mut changed, path, like)?;
        Ok(changed)
    }

    /// Adopt `changed`, recording the step on every new or altered entry.
    pub(crate) fn accept(&mut self, mut changed: PartitionTree, step: &Name) {
        for (path, entry) in &mut changed.entries {
            if !self.tree.entries.contains_key(path) {
                self.derived.entry(path.clone()).or_default();
            }
            if !self.tree.entries.get(path).is_some_and(|previous| {
                previous.kind == entry.kind
                    && previous.metadata == entry.metadata
                    && previous.provenance == entry.provenance
            }) {
                entry.provenance.push(Provenance::Step { id: step.clone() });
            }
        }
        self.tree = changed;
    }
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
    let asset = |path: &RelativePath| text(&std::fs::read(path.resolve(context.assets))?);
    let before = match transform {
        Transform::WriteBytes { before, .. } => Some(before),
        Transform::Xml { before, .. } | Transform::Diff { before, .. } => before.as_ref(),
        _ => None,
    };
    if let Some(before) = before
        && Sha256Digest::bytes(bytes) != *before
    {
        return Err(Error::Before(before.clone()));
    }
    Ok(match transform {
        Transform::WriteBytes { edits, .. } => crate::binary::apply(bytes, edits)?,
        Transform::Xml { edits, .. } => crate::xml::apply(bytes, edits)?,
        Transform::Diff { patch, .. } => {
            crate::text::apply(&text(bytes)?, &asset(patch)?)?.into_bytes()
        }
        Transform::Property {
            key,
            value: Some(value),
        } => android_property::set(&text(bytes)?, key, value, None)?.into_bytes(),
        Transform::Property { key, value: None } => {
            android_property::remove(&text(bytes)?, key, None)?.into_bytes()
        }
        // The string is evaluated before the edit, where expressions are. Appending
        // nothing changes nothing.
        Transform::Append(Expr::Literal(Value::Str(data))) if data.is_empty() => bytes.to_vec(),
        Transform::Append(Expr::Literal(Value::Str(data))) => {
            let mut text = text(bytes)?;
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(data);
            if !data.ends_with('\n') {
                text.push('\n');
            }
            text.into_bytes()
        }
        Transform::Append(_) => {
            return Err(Error::Unsupported("Append's string is evaluated first"));
        }
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
        } => crate::sepolicy::apply(bytes, policies, resolve_with, context)?,
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
