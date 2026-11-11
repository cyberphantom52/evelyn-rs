//! Mutable partition trees. Untouched files keep referring to their source image;
//! every edit writes a new blob, and blobs are never changed in place.

use android_firmware::{AndroidMetadata, Filesystem, Name, RelativePath, Sha256Writer, SourceRef};
use android_fs::{Content, Entry, EntryKind, File, PartitionTree};
use android_fs::{Image, Store};
use recipe::tree::Overrides;
use std::{
    collections::BTreeMap,
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

/// A target entry's metadata.
#[derive(Clone, Debug)]
pub(crate) enum EntryMetadata {
    /// Its own.
    Set(AndroidMetadata),
    /// Android's defaults for where the entry is on the device, as the finished
    /// build has them, then what these set over them, in order.
    Default(Vec<Overrides>),
}

/// A partition tree being edited.
pub(crate) type Tree = PartitionTree<EntryMetadata>;

pub struct Target {
    root: PathBuf,
    pub(crate) tree: Tree,
    next_blob: u64,
}

impl Target {
    /// Start a new target at `output` from a source tree, under a new partition identity.
    /// Nothing is copied: entries keep referring to their source image until edited.
    pub fn create(
        output: &Path,
        tree: PartitionTree,
        partition: Name,
        filesystem: Filesystem,
    ) -> Result<Self, Error> {
        let mut tree = tree.map(|_, _, metadata| Ok::<_, Error>(EntryMetadata::Set(metadata)))?;
        tree.partition = partition;
        tree.filesystem = filesystem;
        fs::create_dir_all(output.join("content"))?;
        Ok(Self {
            root: output.into(),
            tree,
            next_blob: 0,
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
    ) -> Result<(), Error> {
        let kind = self.write_blob(fs::File::open(replacement)?)?;
        let canonical = self.file(path)?.path.clone();
        self.set_content(&canonical, kind);
        Ok(())
    }

    /// Copy a subtree to another path in this target, with the metadata the copy
    /// states.
    pub(crate) fn copy_within(
        &mut self,
        from: &RelativePath,
        to: &RelativePath,
        placing: crate::operations::Placing,
    ) -> Result<(), Error> {
        let selected = crate::layout::subtree(&self.tree, from, to)?;
        // The directories made for it are not in the tree, so not like its own.
        let mut written = self.destination(to, placing, |_| None)?;
        written.extend(selected.keys().cloned());
        self.tree.entries.extend(selected);
        self.give(written, placing.metadata);
        Ok(())
    }

    /// Copy a subtree into another target, with the metadata the copy states;
    /// file contents the source holds are linked or copied into the destination.
    pub(crate) fn copy_into(
        &self,
        destination: &mut Self,
        from: &RelativePath,
        to: &RelativePath,
        placing: crate::operations::Placing,
    ) -> Result<(), Error> {
        let selected = crate::layout::subtree(&self.tree, from, to)?;
        let mut written = destination.destination(to, placing, |dir| {
            crate::layout::directory(&self.tree, dir).cloned()
        })?;
        written.extend(selected.keys().cloned());
        let adopted = destination.adopt(&self.root, selected)?;
        destination.tree.entries.extend(adopted);
        destination.give(written, placing.metadata);
        Ok(())
    }

    /// Give `canonical`, and so all of its hardlinks, new content.
    pub(crate) fn set_content(&mut self, canonical: &RelativePath, kind: EntryKind) {
        if let Some(entry) = self.tree.entries.get_mut(canonical) {
            entry.kind = kind;
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
        source_root: &Path,
        mut entries: BTreeMap<RelativePath, Entry<EntryMetadata>>,
    ) -> Result<BTreeMap<RelativePath, Entry<EntryMetadata>>, Error> {
        for entry in entries.values_mut() {
            if let EntryKind::File {
                content: Content::Blob(blob),
                ..
            } = &mut entry.kind
            {
                let source = source_root.join(PartitionTree::blob_path(blob).as_ref());
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
