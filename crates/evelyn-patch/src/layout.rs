//! Pure, same-partition layout edits. The partition root is implicit and cannot be moved.
//! Every edit keeps hardlink groups intact: when a group's canonical entry is dropped,
//! its lexicographically first surviving member is promoted to canonical.

use android_firmware::{RelativePath, SourceRef};
use android_fs::{Entry, EntryKind, PartitionTree, Provenance};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid path: {0}")]
    Firmware(#[from] android_firmware::Error),
    #[error("path does not exist: {0}")]
    MissingPath(RelativePath),
    #[error("destination already exists: {0}")]
    DestinationExists(RelativePath),
    #[error("destination parent does not exist: {0}")]
    MissingParent(RelativePath),
    #[error("destination parent is not a directory: {0}")]
    NonDirectoryParent(RelativePath),
    #[error("cannot relocate {from} into its own subtree at {to}")]
    OwnSubtree {
        from: RelativePath,
        to: RelativePath,
    },
}

/// Recursively remove an existing entry.
pub fn remove(tree: &PartitionTree, path: &RelativePath) -> Result<PartitionTree, Error> {
    exists(tree, path)?;
    let mut output = tree.clone();
    output
        .entries
        .retain(|candidate, _| !within(candidate, path));
    promote(tree, &mut output.entries, |target| within(target, path));
    Ok(output)
}

/// Copy `from` and its descendants out of `source`, mapped to `to`. Image provenance
/// without a source name is attributed to `identity`.
pub fn select(
    source: &PartitionTree,
    from: &RelativePath,
    to: &RelativePath,
    identity: &SourceRef,
) -> Result<BTreeMap<RelativePath, Entry>, Error> {
    let mut selected = subtree(source, from, to)?;
    for entry in selected.values_mut() {
        for provenance in &mut entry.provenance {
            if let Provenance::Image {
                source: name,
                partition,
                sha256,
                ..
            } = provenance
                && name.is_none()
                && partition == &source.partition
                && sha256 == &source.source_sha256
            {
                *name = Some(identity.source);
            }
        }
    }
    Ok(selected)
}

/// Like [`select`], preserving existing provenance as is.
pub(crate) fn subtree(
    source: &PartitionTree,
    from: &RelativePath,
    to: &RelativePath,
) -> Result<BTreeMap<RelativePath, Entry>, Error> {
    exists(source, from)?;
    let mut selected: BTreeMap<_, _> = source
        .entries
        .iter()
        .filter(|(path, _)| within(path, from))
        .map(|(path, entry)| (path.clone(), entry.clone()))
        .collect();
    promote(source, &mut selected, |target| !within(target, from));
    rebase(selected, from, to)
}

/// Whether `path` has `*` or `?` wildcards. Each matches within one path component.
pub(crate) fn is_glob(path: &RelativePath) -> bool {
    path.as_ref().contains(['*', '?'])
}

/// Entries matching `pattern`, outermost only: a matched directory covers its contents.
pub(crate) fn expand(tree: &PartitionTree, pattern: &RelativePath) -> Vec<RelativePath> {
    let mut matched: Vec<RelativePath> = Vec::new();
    for path in tree.entries.keys() {
        if glob_matches(pattern, path) && !matched.iter().any(|outer| within(path, outer)) {
            matched.push(path.clone());
        }
    }
    matched
}

/// Whether `path` matches `pattern` component by component.
pub(crate) fn glob_matches(pattern: &RelativePath, path: &RelativePath) -> bool {
    let pattern: Vec<_> = pattern.as_ref().split('/').collect();
    let parts: Vec<_> = path.as_ref().split('/').collect();
    parts.len() == pattern.len() && pattern.iter().zip(&parts).all(|(p, n)| wildcard(p, n))
}

pub(crate) fn wildcard(pattern: &str, name: &str) -> bool {
    match pattern.chars().next() {
        None => name.is_empty(),
        Some('*') => (0..=name.len())
            .filter(|i| name.is_char_boundary(*i))
            .any(|i| wildcard(&pattern[1..], &name[i..])),
        Some(c) => {
            name.chars().next().is_some_and(|n| c == '?' || c == n) && {
                let n = name.chars().next().map_or(0, char::len_utf8);
                wildcard(&pattern[c.len_utf8()..], &name[n..])
            }
        }
    }
}

/// Create any missing directories above `path`. Each takes the metadata of the same
/// directory in `like` when it has one, else of its nearest existing ancestor.
pub(crate) fn create_parents(
    tree: &mut PartitionTree,
    path: &RelativePath,
    like: Option<&PartitionTree>,
) -> Result<(), Error> {
    let mut inherited = tree.root.clone();
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
                let metadata = like
                    .and_then(|like| like.entries.get(&dir))
                    .filter(|entry| matches!(entry.kind, EntryKind::Directory))
                    .map_or_else(|| inherited.clone(), |entry| entry.metadata.clone());
                inherited = metadata.clone();
                tree.entries.insert(
                    dir,
                    Entry {
                        kind: EntryKind::Directory,
                        metadata,
                        provenance: Vec::new(),
                    },
                );
            }
        }
    }
    Ok(())
}

fn exists(tree: &PartitionTree, path: &RelativePath) -> Result<(), Error> {
    if tree.entries.contains_key(path) {
        Ok(())
    } else {
        Err(Error::MissingPath(path.clone()))
    }
}

/// Re-point hardlinks whose canonical entry was `dropped`: the first remaining
/// member of each group takes the canonical's kind and the rest link to it.
fn promote(
    source: &PartitionTree,
    entries: &mut BTreeMap<RelativePath, Entry>,
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
                source.entries[target].kind.clone()
            } else {
                EntryKind::Hardlink {
                    target: promoted.clone(),
                }
            };
        }
    }
}

/// Move every path (and hardlink target) under `from` to the same place under `to`.
fn rebase(
    entries: BTreeMap<RelativePath, Entry>,
    from: &RelativePath,
    to: &RelativePath,
) -> Result<BTreeMap<RelativePath, Entry>, Error> {
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
