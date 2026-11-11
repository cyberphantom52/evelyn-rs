use crate::{Error, Target, compose::Context, layout, target::EntryMetadata};
use android_firmware::{AndroidMetadata, Name, RelativePath};
use android_fs::{Entry, EntryKind, PartitionTree};
use recipe::{
    Returns,
    tree::{Expr, Metadata, MethodEdit, Overrides, Transform},
    value::Value,
};
use std::{collections::BTreeSet, path::Path};

impl Target {
    /// Remove `path` (or every entry a glob matches) and its contents.
    pub(crate) fn remove(&mut self, path: &RelativePath) -> Result<(), Error> {
        for path in layout::matches(&self.tree, path) {
            // A match may already be gone with a directory matched before it.
            if self.tree.entries.contains_key(&path) {
                layout::remove(&mut self.tree, &path)?;
            }
        }
        Ok(())
    }

    /// Copy `path` from a firmware's `source` tree, with the metadata the copy
    /// states. With the source's metadata, a directory made for it must be one the
    /// source has, whose metadata it takes.
    pub(crate) fn import(
        &mut self,
        source: &PartitionTree,
        path: &RelativePath,
        to_path: &RelativePath,
        placing: Placing,
    ) -> Result<(), Error> {
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
        for (from_path, to_path) in pairs {
            let selected = layout::subtree(source, &from_path, &to_path)?;
            let mut written = self.destination(&to_path, placing, |dir| {
                layout::directory(source, dir)
                    .cloned()
                    .map(EntryMetadata::Set)
            })?;
            if placing.metadata == Metadata::Source
                && let Some(made) = written
                    .iter()
                    .find(|path| !source.entries.contains_key(*path))
            {
                return Err(layout::Error::MissingPath(made.clone()).into());
            }
            for (path, entry) in selected {
                let entry = Entry {
                    kind: entry.kind,
                    metadata: EntryMetadata::Set(entry.metadata),
                };
                self.tree.entries.insert(path.clone(), entry);
                written.insert(path);
            }
            self.give(written, placing.metadata);
        }
        Ok(())
    }

    /// Write `file` at `to`, with Android's defaults. This replaces the directory
    /// entry, not every alias of its old inode.
    pub(crate) fn install(
        &mut self,
        file: &Path,
        to: &RelativePath,
        placing: Placing,
    ) -> Result<(), Error> {
        let previous = self.tree.entries.get(to);
        if previous.is_some_and(|entry| matches!(entry.kind, EntryKind::Directory)) {
            return Err(Error::Unsupported(
                "remove a directory explicitly before installing a file",
            ));
        }
        let made = self.destination(to, placing, |_| None)?;
        let kind = self.write_blob(std::fs::File::open(file)?)?;
        let metadata = EntryMetadata::Default(Vec::new());
        self.tree
            .entries
            .insert(to.clone(), Entry { kind, metadata });
        self.give(made, placing.metadata);
        Ok(())
    }

    /// Set what `overrides` sets on the entry at `path`; for an entry waiting for
    /// Android's defaults, it is set over them.
    pub(crate) fn set_metadata(
        &mut self,
        path: &RelativePath,
        overrides: &Overrides,
    ) -> Result<(), Error> {
        let entry = self
            .tree
            .entries
            .get_mut(path)
            .ok_or_else(|| Error::Missing(path.clone()))?;
        match &mut entry.metadata {
            EntryMetadata::Set(metadata) => set(metadata, overrides),
            EntryMetadata::Default(over) => over.push(overrides.clone()),
        }
        Ok(())
    }

    /// Have `path` get Android's defaults; what was set on it before no longer
    /// applies.
    pub(crate) fn reset(&mut self, path: &RelativePath) {
        if let Some(entry) = self.tree.entries.get_mut(path) {
            entry.metadata = EntryMetadata::Default(Vec::new());
        }
    }

    /// The finished tree: every entry waiting for them gets Android's defaults
    /// for where it is on the device, from `defaults`, then what was set over them.
    pub(crate) fn resolve(
        self,
        partition: &Name,
        mounts: &android_firmware::Mounts,
        defaults: Option<&crate::defaults::Defaults>,
    ) -> Result<PartitionTree, Error> {
        self.tree.map(|path, kind, metadata| match metadata {
            EntryMetadata::Set(metadata) => Ok(metadata),
            EntryMetadata::Default(overrides) => {
                let at = android_firmware::TargetPath {
                    partition: partition.clone(),
                    path: path.clone(),
                };
                let device = mounts
                    .device_path(&at)
                    .ok_or_else(|| Error::UnknownPartition(partition.clone()))?;
                let defaults = defaults.expect("made when an entry waits for them");
                let mut metadata = defaults.metadata(&device, kind)?;
                for overrides in &overrides {
                    set(&mut metadata, overrides);
                }
                Ok(metadata)
            }
        })
    }

    /// Whether an entry waits for Android's defaults.
    pub(crate) fn waits_for_defaults(&self) -> bool {
        self.tree
            .entries
            .values()
            .any(|entry| matches!(entry.metadata, EntryMetadata::Default(_)))
    }

    /// Transform a file's content for its whole hardlink group, keeping metadata.
    pub(crate) fn transform(
        &mut self,
        path: &RelativePath,
        transform: &Transform,
        context: &Context<'_>,
    ) -> Result<(), Error> {
        let canonical = self.file(path)?.path.clone();
        let bytes = self.read(path, context.images)?;
        let changed = transformed(&bytes, transform, context)?;
        if changed == bytes {
            return Ok(());
        }
        let kind = self.write_blob(changed.as_slice())?;
        self.set_content(&canonical, kind);
        Ok(())
    }

    /// Clear `path` for a new entry: an entry there is removed only to `overwrite`
    /// it, and missing parent directories are made, with the metadata `like`
    /// gives them, only to `create_parent`; the directories made.
    pub(crate) fn destination(
        &mut self,
        path: &RelativePath,
        placing: Placing,
        like: impl Fn(&RelativePath) -> Option<EntryMetadata>,
    ) -> Result<BTreeSet<RelativePath>, Error> {
        if self.tree.entries.contains_key(path) {
            if !placing.overwrite {
                return Err(layout::Error::DestinationExists(path.clone()).into());
            }
            layout::remove(&mut self.tree, path)?;
        }
        if !placing.create_parent
            && let Some((parent, _)) = path.as_ref().rsplit_once('/')
        {
            let parent = RelativePath::try_from(parent.to_owned())?;
            if !self.tree.entries.contains_key(&parent) {
                return Err(layout::Error::MissingParent(parent).into());
            }
        }
        Ok(layout::create_parents(&mut self.tree, path, like)?)
    }

    /// Give what a copy wrote the metadata it states: what the entries came with,
    /// or Android's defaults.
    pub(crate) fn give(&mut self, written: BTreeSet<RelativePath>, metadata: Metadata) {
        if metadata == Metadata::Default {
            for path in written {
                self.reset(&path);
            }
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
    let asset = |expr: &Expr| text(&std::fs::read(context.asset(&asset_path(expr)?)?)?);
    Ok(match transform {
        Transform::WriteBytes(edits) => crate::binary::apply(bytes, edits)?,
        Transform::Xml(edits) => {
            let edits = edits
                .iter()
                .map(|edit| {
                    edit.map(|text| literal(text).map(str::to_owned))?
                        .parse()
                        .map_err(|error| Error::from(recipe::Error::Invalid(error)))
                })
                .collect::<Result<Vec<_>, Error>>()?;
            crate::xml::apply(bytes, &edits)?
        }
        Transform::ApplyDiff(patch) => {
            crate::text::apply(&text(bytes)?, &asset(patch)?)?.into_bytes()
        }
        Transform::Property {
            key,
            value: Some(value),
            create,
        } => {
            let (key, value) = (literal(key)?, literal(value)?);
            if !recipe::property_key(key) || value.contains('\n') {
                return Err(Error::Unsupported(
                    "a property key and value are one line each",
                ));
            }
            android_property::set(&text(bytes)?, key, value, *create).into_bytes()
        }
        Transform::Property {
            key, value: None, ..
        } => android_property::remove(&text(bytes)?, literal(key)?).into_bytes(),
        Transform::Write(text) => literal(text)?.as_bytes().to_vec(),
        Transform::Smali {
            class,
            method,
            edit,
        } => {
            let source = text(bytes)?;
            let (class, method) = (literal(class)?, literal(method)?);
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

/// Set what `overrides` sets on `metadata`.
fn set(metadata: &mut AndroidMetadata, overrides: &Overrides) {
    metadata.uid = overrides.uid.unwrap_or(metadata.uid);
    metadata.gid = overrides.gid.unwrap_or(metadata.gid);
    metadata.mode = overrides.mode.unwrap_or(metadata.mode);
    // The label is evaluated before it is set.
    if let Some(label) = overrides.selinux.as_ref().and_then(Expr::literal_str) {
        metadata.set_selinux(label);
    }
}

/// A transform's string, evaluated before the edit, where expressions are.
fn literal(expr: &Expr) -> Result<&str, Error> {
    expr.literal_str().ok_or(Error::Unsupported(
        "a transform's strings are evaluated first",
    ))
}
