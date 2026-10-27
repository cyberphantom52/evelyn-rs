use crate::{Error, Target, compose::Context, layout};
use model::{
    Entry, EntryKind, Name, PartitionTree, Provenance, RelativePath, Sha256Digest, SourceRef,
};
use recipe::{MetadataPolicy, MetadataRule, Overrides, tree::Transform};
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
    /// target's metadata and new ones the source's, then the overrides apply to
    /// everything it brought in (`mode` to files only).
    pub(crate) fn import(
        &mut self,
        source: &PartitionTree,
        from: &SourceRef,
        path: &RelativePath,
        to_path: &RelativePath,
        item: &Overrides,
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
        let label = item.selinux.as_ref().map(|label| {
            let mut value = label.as_bytes().to_vec();
            value.push(0);
            value
        });
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
                if let Some(uid) = item.uid {
                    entry.metadata.uid = uid;
                }
                if let Some(gid) = item.gid {
                    entry.metadata.gid = gid;
                }
                if let Some(mode) = item.mode
                    && !matches!(entry.kind, EntryKind::Directory)
                {
                    entry.metadata.mode = mode;
                }
                if let Some(label) = &label {
                    entry
                        .metadata
                        .xattrs
                        .insert("security.selinux".into(), label.clone());
                }
            }
            self.accept(changed, step);
        }
        Ok(())
    }

    /// Install an asset at `to`. This replaces the directory entry, not every
    /// alias of its old inode.
    pub(crate) fn install(
        &mut self,
        file: &Path,
        asset: &RelativePath,
        sha256: &Option<Sha256Digest>,
        to: &RelativePath,
        metadata: &MetadataPolicy,
    ) -> Result<PartitionTree, Error> {
        let previous = self.tree.entries.get(to);
        if previous.is_some_and(|entry| matches!(entry.kind, EntryKind::Directory)) {
            return Err(Error::Unsupported(
                "remove a directory explicitly before installing a file",
            ));
        }
        if matches!(metadata, MetadataPolicy::Preserve) && previous.is_some() {
            self.file(to)?;
        }
        let resolved = metadata.resolve(previous.map(|entry| &entry.metadata))?;
        let mut lineage = previous
            .map(|entry| entry.provenance.clone())
            .unwrap_or_default();
        let mut changed = self.destination(to, true, None)?;
        let bytes = std::fs::read(file)?;
        let digest = Sha256Digest::bytes(&bytes);
        if sha256.as_ref().is_some_and(|sha256| *sha256 != digest) {
            return Err(Error::Identity(asset.clone()));
        }
        lineage.push(Provenance::Asset {
            path: asset.clone(),
            sha256: digest,
        });
        changed.entries.insert(
            to.clone(),
            Entry {
                kind: self.write_blob(bytes.as_slice())?,
                metadata: resolved,
                provenance: lineage,
            },
        );
        Ok(changed)
    }

    /// Install every file under `assets` at the same path. Replaced files keep their
    /// metadata; new files take the first matching rule's.
    pub(crate) fn install_tree(
        &mut self,
        recipe_dir: &Path,
        assets: &RelativePath,
        rules: &[MetadataRule],
        step: &Name,
    ) -> Result<(), Error> {
        for (relative, file) in files(&assets.resolve(recipe_dir))? {
            let path = RelativePath::try_from(relative.clone())?;
            let metadata = match self.tree.entries.get(&path) {
                Some(entry) if matches!(entry.kind, EntryKind::Directory) => {
                    return Err(Error::Unsupported(
                        "remove a directory explicitly before installing a file",
                    ));
                }
                Some(entry) => entry.metadata.clone(),
                None => rules
                    .iter()
                    .find(|rule| layout::glob_matches(&rule.path, &path))
                    .ok_or_else(|| Error::NoMetadataRule(path.clone()))?
                    .metadata
                    .resolve()?,
            };
            let bytes = std::fs::read(&file)?;
            let mut changed = self.destination(&path, true, None)?;
            changed.entries.insert(
                path,
                Entry {
                    kind: self.write_blob(bytes.as_slice())?,
                    metadata,
                    provenance: vec![Provenance::Asset {
                        path: RelativePath::try_from(format!("{assets}/{relative}"))?,
                        sha256: Sha256Digest::bytes(&bytes),
                    }],
                },
            );
            self.accept(changed, step);
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
fn files(root: &Path) -> Result<Vec<(String, std::path::PathBuf)>, Error> {
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
        Transform::Bytes { before, edits } => crate::binary::apply(bytes, before.as_ref(), edits)?,
        Transform::Xml { before, edits } => crate::xml::apply(bytes, before.as_ref(), edits)?,
        Transform::TextPatch { patch, before } => {
            crate::text::apply(&text(bytes)?, before.as_ref(), &asset(patch)?)?.into_bytes()
        }
        Transform::SetProp { key, value } => {
            android_property::set(&text(bytes)?, key, value, None)?.into_bytes()
        }
        Transform::DelProp { key } => {
            android_property::remove(&text(bytes)?, key, None)?.into_bytes()
        }
    })
}
