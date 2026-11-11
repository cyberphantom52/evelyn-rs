//! Same-partition layout edits. The partition root is implicit and cannot be moved.
//! Every edit keeps hardlink groups intact: when a group's canonical entry is dropped,
//! its lexicographically first surviving member is promoted to canonical.

use crate::patch::target::{EntryMetadata, Tree};
use android::firmware::RelativePath;
use android::{Entry, EntryKind, PartitionTree};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid path: {0}")]
    Firmware(#[from] android::firmware::Error),
    #[error("path does not exist: {0}")]
    MissingPath(RelativePath),
    #[error("destination already exists: {0}")]
    DestinationExists(RelativePath),
    #[error("destination parent does not exist: {0}")]
    MissingParent(RelativePath),
    #[error("destination parent is not a directory: {0}")]
    NonDirectoryParent(RelativePath),
}

/// Recursively remove an existing entry.
pub(crate) fn remove(tree: &mut Tree, path: &RelativePath) -> Result<(), Error> {
    exists(tree, path)?;
    let removed: BTreeMap<_, _> = tree
        .entries
        .extract_if(.., |candidate, _| within(candidate, path))
        .collect();
    promote(&removed, &mut tree.entries, |target| within(target, path));
    Ok(())
}

/// Copy `from` and its descendants out of `source`, mapped to `to`.
pub(crate) fn subtree<M: Clone>(
    source: &PartitionTree<M>,
    from: &RelativePath,
    to: &RelativePath,
) -> Result<BTreeMap<RelativePath, Entry<M>>, Error> {
    exists(source, from)?;
    let mut selected: BTreeMap<_, _> = source
        .entries
        .iter()
        .filter(|(path, _)| within(path, from))
        .map(|(path, entry)| (path.clone(), entry.clone()))
        .collect();
    promote(&source.entries, &mut selected, |target| {
        !within(target, from)
    });
    rebase(selected, from, to)
}

/// Whether `path` has `*` or `?` wildcards. Each matches within one path component.
pub(crate) fn is_glob(path: &RelativePath) -> bool {
    path.as_ref().contains(['*', '?'])
}

/// The entries `path` names in a tree: every match of a glob, else the entry if
/// it is there.
pub(crate) fn matches<M>(tree: &PartitionTree<M>, path: &RelativePath) -> Vec<RelativePath> {
    let children = |dir: &str| {
        let prefix = match dir {
            "" => String::new(),
            dir => format!("{dir}/"),
        };
        tree.entries
            .keys()
            .map(|path| path.as_ref())
            .filter_map(|path| path.strip_prefix(prefix.as_str()))
            .filter(|rest| !rest.is_empty() && !rest.contains('/'))
            .map(str::to_owned)
            .collect()
    };
    walk(path.as_ref(), children, |path| {
        RelativePath::try_from(path.to_owned()).is_ok_and(|path| tree.entries.contains_key(&path))
    })
    .into_iter()
    .filter_map(|path| RelativePath::try_from(path).ok())
    .collect()
}

/// What `pattern`, a relative path, matches below a root, in path order: `*` and
/// `?` match within one name, and a segment without them names the one entry.
/// Only the directories a glob needs are listed: `children` names what is in a
/// directory (the root is `""`), and `exists` says whether a path is there.
pub(crate) fn walk(
    pattern: &str,
    children: impl Fn(&str) -> Vec<String>,
    exists: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut found = vec![String::new()];
    for segment in pattern.split('/') {
        let mut next = Vec::new();
        for dir in found {
            let join = |name: &str| match dir.is_empty() {
                true => name.to_owned(),
                false => format!("{dir}/{name}"),
            };
            if segment.contains(['*', '?']) {
                for name in children(&dir) {
                    if android::firmware::glob(segment, &name) {
                        next.push(join(&name));
                    }
                }
            } else if exists(&join(segment)) {
                next.push(join(segment));
            }
        }
        found = next;
    }
    found.sort();
    found
}

/// The metadata of the directory at `path` in `tree`, if it is one.
pub(crate) fn directory<'a, M>(tree: &'a PartitionTree<M>, path: &RelativePath) -> Option<&'a M> {
    tree.entries
        .get(path)
        .filter(|entry| matches!(entry.kind, EntryKind::Directory))
        .map(|entry| &entry.metadata)
}

/// Create any missing directories above `path`; those made. Each takes the metadata
/// `like` gives the same directory when it gives one, else of its nearest existing
/// ancestor.
pub(crate) fn create_parents(
    tree: &mut Tree,
    path: &RelativePath,
    like: impl Fn(&RelativePath) -> Option<EntryMetadata>,
) -> Result<BTreeSet<RelativePath>, Error> {
    let mut made = BTreeSet::new();
    let mut inherited = EntryMetadata::Set(tree.root.clone());
    let mut prefix = String::new();
    let components: Vec<_> = path.as_ref().split('/').collect();
    for component in &components[..components.len() - 1] {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(component);
        let dir = RelativePath::try_from(prefix.clone())?;
        match tree.entries.get(&dir) {
            Some(entry) if matches!(entry.kind, EntryKind::Directory) => {
                inherited = entry.metadata.clone();
            }
            Some(_) => return Err(Error::NonDirectoryParent(dir)),
            None => {
                let metadata = like(&dir).unwrap_or_else(|| inherited.clone());
                inherited = metadata.clone();
                made.insert(dir.clone());
                tree.entries.insert(
                    dir,
                    Entry {
                        kind: EntryKind::Directory,
                        metadata,
                    },
                );
            }
        }
    }
    Ok(made)
}

fn exists<M>(tree: &PartitionTree<M>, path: &RelativePath) -> Result<(), Error> {
    if tree.entries.contains_key(path) {
        Ok(())
    } else {
        Err(Error::MissingPath(path.clone()))
    }
}

/// Re-point hardlinks whose canonical entry was `dropped`: the first remaining
/// member of each group takes the canonical's kind, as `source` has it, and the
/// rest link to it.
fn promote<M>(
    source: &BTreeMap<RelativePath, Entry<M>>,
    entries: &mut BTreeMap<RelativePath, Entry<M>>,
    dropped: impl Fn(&RelativePath) -> bool,
) {
    let mut promotions = BTreeMap::new();
    for (path, entry) in entries {
        if let EntryKind::Hardlink { target } = &entry.kind
            && dropped(target)
        {
            let promoted = promotions
                .entry(target.clone())
                .or_insert_with(|| path.clone());
            entry.kind = if path == promoted {
                source[target].kind.clone()
            } else {
                EntryKind::Hardlink {
                    target: promoted.clone(),
                }
            };
        }
    }
}

/// Move every path (and hardlink target) under `from` to the same place under `to`.
fn rebase<M>(
    entries: BTreeMap<RelativePath, Entry<M>>,
    from: &RelativePath,
    to: &RelativePath,
) -> Result<BTreeMap<RelativePath, Entry<M>>, Error> {
    let moved = |path: &RelativePath| -> Result<RelativePath, Error> {
        if within(path, from) {
            let suffix = &path.as_ref()[from.as_ref().len()..];
            Ok(RelativePath::try_from(format!("{to}{suffix}"))?)
        } else {
            Ok(path.clone())
        }
    };
    entries
        .into_iter()
        .map(|(path, mut entry)| {
            if let EntryKind::Hardlink { target } = &mut entry.kind {
                *target = moved(target)?;
            }
            Ok((moved(&path)?, entry))
        })
        .collect()
}

fn within(path: &RelativePath, root: &RelativePath) -> bool {
    path == root
        || path
            .as_ref()
            .strip_prefix(root.as_ref())
            .is_some_and(|suffix| suffix.starts_with('/'))
}
