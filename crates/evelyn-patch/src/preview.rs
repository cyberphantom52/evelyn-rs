//! Semantic comparison of two partition trees.

use android_firmware::{Name, RelativePath};
use android_fs::{EntryKind, PartitionTree};
use serde::Serialize;
use std::{collections::BTreeMap, mem::discriminant};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("cannot compare different partitions: {before} and {after}")]
    PartitionMismatch { before: Name, after: Name },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Changes {
    pub added: Vec<RelativePath>,
    pub removed: Vec<RelativePath>,
    pub modified: Vec<ModifiedEntry>,
    pub root_metadata: bool,
    pub filesystem: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ModifiedEntry {
    pub path: RelativePath,
    pub content: bool,
    pub metadata: bool,
    pub kind: bool,
    pub links: bool,
}

/// Compare logical content and inode membership, ignoring blob names and provenance.
pub fn compare(before: &PartitionTree, after: &PartitionTree) -> Result<Changes, Error> {
    if before.partition != after.partition {
        return Err(Error::PartitionMismatch {
            before: before.partition.clone(),
            after: after.partition.clone(),
        });
    }
    let before_groups = groups(before);
    let after_groups = groups(after);
    let mut changes = Changes {
        root_metadata: before.root != after.root,
        filesystem: before.filesystem != after.filesystem,
        ..Changes::default()
    };
    for (path, old) in &before.entries {
        let Some(new) = after.entries.get(path) else {
            changes.removed.push(path.clone());
            continue;
        };
        let old_kind = effective_kind(before, &old.kind);
        let new_kind = effective_kind(after, &new.kind);
        let content = match (old_kind, new_kind) {
            (
                EntryKind::File {
                    sha256: a, size: x, ..
                },
                EntryKind::File {
                    sha256: b, size: y, ..
                },
            ) => a != b || x != y,
            (EntryKind::Symlink { target: a }, EntryKind::Symlink { target: b }) => a != b,
            (EntryKind::Directory, EntryKind::Directory) => false,
            _ => true,
        };
        let modified = ModifiedEntry {
            path: path.clone(),
            content,
            metadata: old.metadata != new.metadata,
            kind: discriminant(old_kind) != discriminant(new_kind),
            links: before_groups.get(path) != after_groups.get(path),
        };
        if modified.content || modified.metadata || modified.kind || modified.links {
            changes.modified.push(modified);
        }
    }
    changes.added = after
        .entries
        .keys()
        .filter(|path| !before.entries.contains_key(*path))
        .cloned()
        .collect();
    Ok(changes)
}

fn effective_kind<'a>(tree: &'a PartitionTree, kind: &'a EntryKind) -> &'a EntryKind {
    match kind {
        EntryKind::Hardlink { target } => &tree.entries[target].kind,
        kind => kind,
    }
}

/// Each linkable path's sorted hardlink group, so changed membership is visible
/// regardless of which member is canonical.
fn groups(tree: &PartitionTree) -> BTreeMap<&RelativePath, Vec<&RelativePath>> {
    let mut members = BTreeMap::<&RelativePath, Vec<&RelativePath>>::new();
    for (path, entry) in &tree.entries {
        let canonical = match &entry.kind {
            EntryKind::Directory => continue,
            EntryKind::Hardlink { target } => target,
            _ => path,
        };
        members.entry(canonical).or_default().push(path);
    }
    members
        .into_values()
        .flat_map(|group| {
            group
                .clone()
                .into_iter()
                .map(move |path| (path, group.clone()))
        })
        .collect()
}
