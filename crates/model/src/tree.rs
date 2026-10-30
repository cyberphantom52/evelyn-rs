use crate::{Content, Entry, EntryKind, Error};
use android_firmware::{AndroidMetadata, Filesystem, Name, RelativePath, Sha256Digest};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// A partition's entries with their Android metadata. File bytes stay in the source
/// image until an edit writes a blob into the tree's `content/` directory.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartitionTree {
    pub schema: u32,
    pub partition: Name,
    pub filesystem: Filesystem,
    pub source_sha256: Sha256Digest,
    pub root: AndroidMetadata,
    pub entries: BTreeMap<RelativePath, Entry>,
}

impl PartitionTree {
    pub fn validate(&self) -> Result<(), Error> {
        let invalid = |value: String| Error::Invalid {
            kind: "partition tree",
            value,
        };
        if self.schema != 2 {
            return Err(invalid(format!("unsupported schema {}", self.schema)));
        }
        let mut blobs = BTreeSet::new();
        for (path, entry) in &self.entries {
            if let Some((parent, _)) = path.as_ref().rsplit_once('/') {
                let parent = RelativePath::try_from(parent.to_owned())?;
                if !self
                    .entries
                    .get(&parent)
                    .is_some_and(|entry| matches!(entry.kind, EntryKind::Directory))
                {
                    return Err(invalid(format!("missing directory parent for {path}")));
                }
            }
            match &entry.kind {
                EntryKind::File {
                    content: Content::Blob(blob),
                    ..
                } if !blobs.insert(blob) => {
                    return Err(invalid(format!(
                        "duplicate writable blob {blob}; use an explicit hardlink"
                    )));
                }
                EntryKind::Symlink { target } if target.is_empty() || target.contains(&0) => {
                    return Err(invalid(format!("invalid symlink target at {path}")));
                }
                EntryKind::Hardlink { target } => {
                    let canonical = self
                        .entries
                        .get(target)
                        .ok_or_else(|| invalid(format!("missing hardlink target {target}")))?;
                    if path == target
                        || !matches!(
                            canonical.kind,
                            EntryKind::File { .. } | EntryKind::Symlink { .. }
                        )
                        || canonical.metadata != entry.metadata
                    {
                        return Err(invalid(format!(
                            "invalid or inconsistent hardlink {path} -> {target}"
                        )));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn blob_path(blob: &Name) -> RelativePath {
        RelativePath::try_from(format!("content/{blob}"))
            .expect("validated blob names are safe components")
    }
}
