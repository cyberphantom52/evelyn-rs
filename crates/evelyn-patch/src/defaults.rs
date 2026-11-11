//! The metadata a new entry gets: Android's defaults for its device path, from the
//! finished build's own `fs_config_*` files and `file_contexts`, as the device
//! would label it.

use crate::{Composition, Error, Images, Target};
use android::EntryKind;
use android::defaults::{FileContexts, FileKind, FsConfig, capability_xattr};
use android::firmware::{
    AndroidMetadata, DevicePath, FILE_CONTEXTS, FS_CONFIG, Mounts, Name, TargetPath,
};
use std::collections::BTreeMap;

pub(crate) struct Defaults {
    fs_config: FsConfig,
    contexts: FileContexts,
}

impl Defaults {
    /// The defaults as the build has them.
    pub(crate) fn new(
        targets: &BTreeMap<Name, Target>,
        mounts: &Mounts,
        images: &Images,
    ) -> Result<Self, Error> {
        let read = |path: &str| -> Result<Option<Vec<u8>>, Error> {
            let path = DevicePath::try_from(path.to_owned())?;
            let Ok([at]) =
                <[TargetPath; 1]>::try_from(Composition::resolve(targets, mounts, &path))
            else {
                return Ok(None);
            };
            match targets.get(&at.partition) {
                Some(target) if target.file(&at.path).is_ok() => {
                    target.read(&at.path, images).map(Some)
                }
                _ => Ok(None),
            }
        };
        let mut files = Vec::new();
        let mut dirs = Vec::new();
        for root in FS_CONFIG {
            files.extend(read(&format!("{root}/etc/fs_config_files"))?);
            dirs.extend(read(&format!("{root}/etc/fs_config_dirs"))?);
        }
        let mut contexts = Vec::new();
        for path in FILE_CONTEXTS {
            if let Some(bytes) = read(path)? {
                contexts.push(
                    String::from_utf8(bytes)
                        .map_err(|_| Error::Unsupported("file_contexts must be UTF-8"))?,
                );
            }
        }
        let files: Vec<&[u8]> = files.iter().map(Vec::as_slice).collect();
        let dirs: Vec<&[u8]> = dirs.iter().map(Vec::as_slice).collect();
        let contexts: Vec<&str> = contexts.iter().map(String::as_str).collect();
        Ok(Self {
            fs_config: FsConfig::new(&files, &dirs)?,
            contexts: FileContexts::new(&contexts)?,
        })
    }

    /// Android's metadata for an entry of `kind` at `path`.
    pub(crate) fn metadata(
        &self,
        path: &DevicePath,
        kind: &EntryKind,
    ) -> Result<AndroidMetadata, Error> {
        let (file_kind, dir) = match kind {
            EntryKind::Directory => (FileKind::Directory, true),
            EntryKind::Symlink { .. } => (FileKind::Symlink, false),
            EntryKind::File { .. } | EntryKind::Hardlink { .. } => (FileKind::File, false),
        };
        let path = path.to_string();
        let ownership = self.fs_config.get(path.trim_start_matches('/'), dir);
        let label = self
            .contexts
            .label(&path, file_kind)
            .ok_or_else(|| Error::NoLabel(path.clone()))?;
        let mut metadata = AndroidMetadata {
            uid: ownership.uid,
            gid: ownership.gid,
            mode: ownership.mode,
            xattrs: BTreeMap::new(),
        };
        metadata.set_selinux(label);
        if let Some(capabilities) = capability_xattr(ownership.capabilities) {
            metadata
                .xattrs
                .insert("security.capability".to_owned(), capabilities);
        }
        Ok(metadata)
    }
}
