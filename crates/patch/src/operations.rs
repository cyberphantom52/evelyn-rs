use crate::{Error, Target, compose::Context, layout};
use model::{
    Entry, EntryKind, Name, PartitionTree, Provenance, RelativePath, Sha256Digest, SourceRef,
};
use recipe::{Data, Metadata, Overrides, Returns, tree::Transform};
use std::path::Path;

impl Target {
    /// Move a subtree within this target.
    pub(crate) fn relocate(
        &mut self,
        from: &RelativePath,
        to: &RelativePath,
        step: &Name,
    ) -> Result<(), Error> {
        let changed = layout::relocate(&self.tree, from, to)?;
        self.accept(changed, step);
        Ok(())
    }

    /// Remove `path` (or every entry a glob matches) and its contents. Returns
    /// whether there was anything to remove.
    pub(crate) fn remove(&mut self, path: &RelativePath, step: &Name) -> Result<bool, Error> {
        let matches = if layout::is_glob(path) {
            layout::expand(&self.tree, path)
        } else if self.tree.entries.contains_key(path) {
            vec![path.clone()]
        } else {
            Vec::new()
        };
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

    /// Import `path` from a firmware's `source` tree: overwritten entries keep this
    /// target's metadata; new ones have the source's, or with `Default` get
    /// Android's defaults when the build is done.
    pub(crate) fn import(
        &mut self,
        source: &PartitionTree,
        from: &SourceRef,
        path: &RelativePath,
        to_path: &RelativePath,
        metadata: Metadata,
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
            let earlier = self.created.clone();
            self.accept(changed, step);
            if metadata == Metadata::Default {
                continue;
            }
            // What the source has keeps the source's metadata: what it brought, and
            // the parent directories made like the source's.
            self.created.retain(|path| {
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

    /// Set what `overrides` sets on the entry at `path`; whether that changed it.
    pub(crate) fn set_metadata(
        &mut self,
        path: &RelativePath,
        overrides: &Overrides,
        step: &Name,
    ) -> Result<bool, Error> {
        let entry = self
            .tree
            .entries
            .get_mut(path)
            .ok_or_else(|| Error::Missing(path.clone()))?;
        let mut metadata = entry.metadata.clone();
        metadata.uid = overrides.uid.unwrap_or(metadata.uid);
        metadata.gid = overrides.gid.unwrap_or(metadata.gid);
        metadata.mode = overrides.mode.unwrap_or(metadata.mode);
        // A label is the same with or without its terminating NUL, as for the kernel.
        let current = metadata.xattrs.get("security.selinux");
        if let Some(label) = &overrides.selinux
            && current.is_none_or(|current| {
                current.strip_suffix(&[0]).unwrap_or(current) != label.as_bytes()
            })
        {
            let mut value = label.as_bytes().to_vec();
            value.push(0);
            metadata.xattrs.insert("security.selinux".into(), value);
        }
        if metadata == entry.metadata {
            return Ok(false);
        }
        entry.metadata = metadata;
        entry.provenance.push(Provenance::Step { id: step.clone() });
        Ok(true)
    }

    /// Give every new entry Android's defaults for where it is on the device.
    pub(crate) fn give_defaults(
        &mut self,
        partition: &Name,
        mounts: &model::Mounts,
        defaults: &crate::defaults::Defaults,
    ) -> Result<(), Error> {
        for path in std::mem::take(&mut self.created) {
            let Some(entry) = self.tree.entries.get_mut(&path) else {
                continue;
            };
            let at = model::TargetPath {
                partition: partition.clone(),
                path: path.clone(),
            };
            let device = mounts
                .device_path(&at)
                .ok_or_else(|| Error::UnknownPartition(partition.clone()))?;
            entry.metadata = defaults.metadata(&device, &entry.kind)?;
        }
        Ok(())
    }

    /// Transform a file's content for its whole hardlink group, keeping metadata.
    pub(crate) fn transform(
        &mut self,
        path: &RelativePath,
        transform: &Transform,
        context: &Context<'_>,
        step: &Name,
    ) -> Result<(), Error> {
        let canonical = self.file(path)?.0.clone();
        let bytes = self.read(path, context.images)?;
        let changed = transformed(&bytes, transform, context.assets)?;
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
                self.created.insert(path.clone());
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

/// Regular files under `root`, as (path relative to `root`, full path), sorted.
pub(crate) fn files(root: &Path) -> Result<Vec<(String, std::path::PathBuf)>, Error> {
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
                return Err(Error::Unsupported(
                    "asset trees may hold only files and directories",
                ));
            }
        }
    }
    found.sort();
    Ok(found)
}

/// A file's content after `transform`, wherever the file is: in the build or in a
/// package.
pub(crate) fn transformed(
    bytes: &[u8],
    transform: &Transform,
    assets: &Path,
) -> Result<Vec<u8>, Error> {
    let text = |bytes: &[u8]| -> Result<String, Error> {
        String::from_utf8(bytes.to_vec())
            .map_err(|_| Error::Unsupported("text edits require UTF-8"))
    };
    let asset = |path: &RelativePath| text(&std::fs::read(path.resolve(assets))?);
    Ok(match transform {
        Transform::Bytes { before, edits } => crate::binary::apply(bytes, before, edits)?,
        Transform::Xml { before, edits } => crate::xml::apply(bytes, before.as_ref(), edits)?,
        Transform::TextPatch { patch, before } => {
            crate::text::apply(&text(bytes)?, before.as_ref(), &asset(patch)?)?.into_bytes()
        }
        Transform::SetProp { key, value } => {
            android_property::set(&text(bytes)?, key, value, None)?.into_bytes()
        }
        Transform::Append(Data::Text(data)) => {
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
        Transform::Append(Data::File { .. }) => {
            return Err(Error::Unsupported(
                "a firmware file is read before it is appended",
            ));
        }
        Transform::DelProp { key } => {
            android_property::remove(&text(bytes)?, key, None)?.into_bytes()
        }
        Transform::StubMethod {
            class,
            method,
            returns,
        } => {
            let stub = match returns {
                Returns::Void => smali::Stub::Void,
                Returns::Zero => smali::Stub::Zero,
                Returns::One => smali::Stub::One,
            };
            smali::stub_method(&text(bytes)?, class, method, stub)?.into_bytes()
        }
        Transform::ReplaceMethod {
            class,
            method,
            body,
        } => smali::replace_method(&text(bytes)?, class, method, &asset(body)?)?.into_bytes(),
        Transform::InsertMethod {
            class,
            method,
            body,
        } => smali::insert_method(&text(bytes)?, class, method, &asset(body)?)?.into_bytes(),
    })
}
