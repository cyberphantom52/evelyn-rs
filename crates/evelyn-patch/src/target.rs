//! Mutable partition trees. Untouched files keep referring to their source image;
//! every edit writes a new blob, and blobs are never changed in place.

use android_firmware::{Filesystem, Name, RelativePath, Sha256Writer, SourceRef};
use android_fs::{Content, Entry, EntryKind, File, PartitionTree, Provenance};
use android_fs::{Image, Store};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Recipe(#[from] recipe::Error),
    #[error(transparent)]
    Firmware(#[from] android_firmware::Error),
    #[error(transparent)]
    Android(#[from] android_fs::Error),
    #[error(transparent)]
    Align(#[from] crate::align::Error),
    #[error(transparent)]
    Sepolicy(#[from] crate::sepolicy::Error),
    #[error(transparent)]
    Binary(#[from] crate::binary::Error),
    #[error(transparent)]
    Text(#[from] crate::text::Error),
    #[error(transparent)]
    Xml(#[from] crate::xml::Error),
    #[error(transparent)]
    Layout(#[from] crate::layout::Error),
    #[error("target IO: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Package(#[from] crate::package::Error),
    #[error(transparent)]
    Smali(#[from] smali::Error),
    #[error(transparent)]
    Tool(#[from] toolchain::Error),
    #[error(transparent)]
    Value(#[from] recipe::value::ValueError),
    #[error("step {step}: {message}")]
    Assertion { step: Name, message: String },
    #[error("no target partition {0}")]
    UnknownPartition(Name),
    #[error("step {step} removes from a scope that selects no partition of this build")]
    EmptyScope { step: Name },
    #[error("step {step} touches {path}, which is in no partition this build has")]
    AbsentPath {
        step: Name,
        path: android_firmware::DevicePath,
    },
    #[error("source {0} was not extracted")]
    UnknownSource(android_firmware::SourceRef),
    #[error(transparent)]
    Defaults(#[from] android_fs::defaults::Error),
    #[error("no file_contexts label for new entry {0}")]
    NoLabel(String),
    #[error("missing target entry {0}")]
    Missing(RelativePath),
    #[error("unsupported target transformation: {0}")]
    Unsupported(&'static str),
    #[error("{path}: {source}")]
    At {
        path: String,
        #[source]
        source: Box<Error>,
    },
}

/// Open source images, by source reference.
pub type Images = BTreeMap<SourceRef, Image>;

pub struct Target {
    root: PathBuf,
    pub(crate) tree: PartitionTree,
    next_blob: u64,
    /// Entries that get Android's defaults when the build is done, with what is
    /// set over them.
    pub(crate) derived: BTreeMap<RelativePath, Vec<crate::operations::Pending>>,
}

impl Target {
    /// Start a new target at `output` from a source tree, under a new partition identity.
    /// Nothing is copied: entries keep referring to their source image until edited.
    pub fn create(
        output: &Path,
        mut tree: PartitionTree,
        partition: Name,
        filesystem: Filesystem,
    ) -> Result<Self, Error> {
        tree.partition = partition;
        tree.filesystem = filesystem;
        fs::create_dir_all(output.join("content"))?;
        Ok(Self {
            root: output.into(),
            tree,
            next_blob: 0,
            derived: BTreeMap::new(),
        })
    }

    /// Reads this target's file contents from its blobs and the open source images.
    pub fn store<'a>(&'a self, images: &'a Images) -> Store<'a> {
        Store::new(images, &self.root)
    }

    /// An on-disk copy of a regular file (or hardlink to one),
    /// for tools that need a path. Image contents are written out as a blob first.
    pub(crate) fn content_path(
        &mut self,
        path: &RelativePath,
        images: &Images,
    ) -> Result<PathBuf, Error> {
        let file = self.file(path)?;
        let content = match file.content {
            Content::Blob(blob) => return Ok(self.blob_path(blob)),
            content => content.clone(),
        };
        let (canonical, sha256, size) = (file.path.clone(), file.sha256.clone(), file.size);
        let blob = self.new_blob();
        let mut output = fs::File::create_new(self.blob_path(&blob))?;
        io::copy(&mut self.store(images).open(&content, size)?, &mut output)?;
        if let Some(entry) = self.tree.entries.get_mut(&canonical) {
            entry.kind = EntryKind::File {
                content: Content::Blob(blob.clone()),
                sha256,
                size,
            };
        }
        Ok(self.blob_path(&blob))
    }

    /// Replace a file (and all of its hardlinks) with the contents of `replacement`.
    pub(crate) fn replace_file(
        &mut self,
        path: &RelativePath,
        replacement: &Path,
        step: &Name,
    ) -> Result<(), Error> {
        let kind = self.write_blob(fs::File::open(replacement)?)?;
        let canonical = self.file(path)?.path.clone();
        self.accept_content(&canonical, kind, step);
        Ok(())
    }

    /// Copy a subtree to another path in this target, with its metadata; the
    /// paths written.
    pub(crate) fn copy_within(
        &mut self,
        from: &RelativePath,
        to: &RelativePath,
        placing: crate::operations::Placing,
        step: &Name,
    ) -> Result<BTreeSet<RelativePath>, Error> {
        let selected = crate::layout::subtree(&self.tree, from, to)?;
        let mut changed = self.destination(to, placing, Some(&self.tree))?;
        changed.entries.extend(selected);
        Ok(self.accept(changed, step))
    }

    /// Copy a subtree into another target, keeping its metadata; file contents the
    /// source holds are linked or copied into the destination.
    pub(crate) fn copy_into(
        &self,
        destination: &mut Self,
        from: &RelativePath,
        to: &RelativePath,
        placing: crate::operations::Placing,
        step: &Name,
    ) -> Result<BTreeSet<RelativePath>, Error> {
        let selected = crate::layout::subtree(&self.tree, from, to)?;
        let mut changed = destination.destination(to, placing, Some(&self.tree))?;
        changed
            .entries
            .extend(destination.adopt(Some(&self.root), selected)?);
        Ok(destination.accept(changed, step))
    }

    /// Write the manifest and drop blobs no longer referenced by the tree.
    pub fn finish(self) -> Result<PartitionTree, Error> {
        android_fs::write_tree(&self.root, &self.tree)?;
        let live: BTreeSet<_> = self
            .tree
            .entries
            .values()
            .filter_map(|entry| match &entry.kind {
                EntryKind::File {
                    content: Content::Blob(blob),
                    ..
                } => Some(blob.as_ref()),
                _ => None,
            })
            .collect();
        for entry in fs::read_dir(self.root.join("content"))? {
            let entry = entry?;
            if !live.contains(entry.file_name().to_string_lossy().as_ref()) {
                fs::remove_file(entry.path())?;
            }
        }
        Ok(self.tree)
    }

    /// Give `canonical` new content and record the step on it and all of its hardlinks.
    pub(crate) fn accept_content(
        &mut self,
        canonical: &RelativePath,
        kind: EntryKind,
        step: &Name,
    ) {
        for (path, entry) in &mut self.tree.entries {
            if path == canonical {
                entry.kind = kind.clone();
            } else if !matches!(&entry.kind, EntryKind::Hardlink { target } if target == canonical)
            {
                continue;
            }
            entry.provenance.push(Provenance::Step { id: step.clone() });
        }
    }

    /// A regular file, following a hardlink to its canonical entry.
    pub(crate) fn file(&self, path: &RelativePath) -> Result<File<'_>, Error> {
        Ok(self.tree.file(path)?)
    }

    pub(crate) fn read(&self, path: &RelativePath, images: &Images) -> Result<Vec<u8>, Error> {
        Ok(self.store(images).read(&self.file(path)?)?)
    }

    /// Write a new blob from `reader`.
    pub(crate) fn write_blob(&mut self, mut reader: impl Read) -> Result<EntryKind, Error> {
        let blob = self.new_blob();
        let mut writer = Sha256Writer::new(fs::File::create_new(self.blob_path(&blob))?);
        let size = io::copy(&mut reader, &mut writer)?;
        let (_, sha256) = writer.finish()?;
        Ok(EntryKind::File {
            content: Content::Blob(blob),
            sha256,
            size,
        })
    }

    /// Bring entries into this target. Image contents are shared as they are; blobs of
    /// the tree at `source_root` are linked (or copied) into this target's store.
    pub(crate) fn adopt(
        &mut self,
        source_root: Option<&Path>,
        mut entries: BTreeMap<RelativePath, Entry>,
    ) -> Result<BTreeMap<RelativePath, Entry>, Error> {
        for entry in entries.values_mut() {
            if let EntryKind::File {
                content: Content::Blob(blob),
                ..
            } = &mut entry.kind
            {
                let root = source_root.ok_or(Error::Unsupported("blob outside a target"))?;
                let source = root.join(PartitionTree::blob_path(blob).as_ref());
                *blob = self.new_blob();
                let destination = self.blob_path(blob);
                if fs::hard_link(&source, &destination).is_err() {
                    fs::copy(&source, &destination)?;
                }
            }
        }
        Ok(entries)
    }

    fn new_blob(&mut self) -> Name {
        self.next_blob += 1;
        Name::try_from(format!("t{}", self.next_blob)).expect("blob names are valid")
    }

    fn blob_path(&self, blob: &Name) -> PathBuf {
        self.root.join(PartitionTree::blob_path(blob).as_ref())
    }
}
