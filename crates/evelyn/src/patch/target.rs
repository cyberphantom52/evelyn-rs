//! Mutable partition trees. Untouched files keep referring to their source image;
//! every edit writes a new blob, and blobs are never changed in place.

use crate::patch::layout;
use crate::patch::{package::Package, sepolicy::Document};
use crate::recipe::Metadata;
use android::Filesystem;
use android::metadata::AndroidMetadata;
use android::path::RelativePath;
use android::{Content, Entry, EntryKind, File, PartitionTree};
use android::{Image, Store};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};
use thiserror::Error;

/// An I/O result with the path it was about attached to its error.
pub(crate) trait IoAt<T> {
    fn at(self, path: impl AsRef<Path>) -> Result<T, Error>;
}

impl<T> IoAt<T> for io::Result<T> {
    fn at(self, path: impl AsRef<Path>) -> Result<T, Error> {
        self.map_err(|source| Error::Io {
            path: path.as_ref().to_path_buf(),
            source,
        })
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Recipe(#[from] crate::recipe::Error),
    #[error(transparent)]
    Android(#[from] android::Error),
    #[error(transparent)]
    Sepolicy(#[from] crate::patch::sepolicy::Error),
    #[error(transparent)]
    Binary(#[from] crate::patch::binary::Error),
    #[error(transparent)]
    Text(#[from] crate::patch::text::Error),
    #[error(transparent)]
    Xml(#[from] crate::patch::xml::Error),
    #[error(transparent)]
    Layout(#[from] layout::Error),
    #[error("cannot access {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Package(#[from] crate::patch::package::Error),
    #[error(transparent)]
    Code(#[from] crate::patch::code::Error),
    #[error(transparent)]
    Tool(#[from] crate::toolchain::Error),
    #[error(transparent)]
    Value(#[from] crate::patch::value::ValueError),
    #[error("step {step}")]
    Step {
        step: String,
        #[source]
        source: Box<Error>,
    },
    #[error(
        "{target}{}",
        input.as_ref().map(|input| format!(", in what {input} added")).unwrap_or_default()
    )]
    Policies {
        target: String,
        /// The input libsepol could not compile a statement of, when it said.
        input: Option<String>,
        #[source]
        source: crate::patch::sepolicy::Error,
    },
    #[error("{0}")]
    Assertion(String),
    #[error("{0} is not set")]
    Unset(String),
    #[error("{call}")]
    Call {
        call: String,
        #[source]
        source: Box<Error>,
    },
    #[error("Remove takes a path from the root, /…, not {0}")]
    RelativeRemove(String),
    #[error("{0} does not work inside a Package")]
    OutsidePackage(String),
    #[error("no target partition {0}")]
    UnknownPartition(String),
    #[error("a scope selects no partition of this build")]
    EmptyScope,
    #[error("{0} is in no partition this build has")]
    AbsentPath(android::path::DevicePath),
    #[error("source {0} was not extracted")]
    UnknownSource(crate::SourceRef),
    #[error(transparent)]
    Defaults(#[from] android::defaults::Error),
    #[error("no file_contexts label for new entry {0}")]
    NoLabel(String),
    #[error("missing target entry {0}")]
    Missing(RelativePath),
    #[error("unsupported target transformation: {0}")]
    Unsupported(&'static str),
    #[error("{path}")]
    At {
        path: String,
        #[source]
        source: Box<Error>,
    },
}

/// The open source images, which scanned trees refer to by index.
pub type Images = Vec<Image>;

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
    /// Files open for the edits of their kind, by canonical path. An open file is
    /// its file's content: reading the file renders it, anything else that needs
    /// the file's bytes writes it into the file first, and what replaces the
    /// file drops it.
    open: BTreeMap<RelativePath, Open>,
}

/// A file open for the edits of its kind, which hold it in memory until the build
/// is written.
pub(crate) enum Open {
    Policy(Document),
    Package(Box<Package>),
}

impl Open {
    /// The file's content, as the edits so far left it.
    fn render(&self) -> Result<Vec<u8>, Error> {
        Ok(match self {
            Open::Policy(document) => document.render()?.into_bytes(),
            Open::Package(package) => {
                let mut bytes = Vec::new();
                package.render(&mut bytes)?;
                bytes
            }
        })
    }
}

impl Target {
    /// Start a new target at `output` from a source tree, built as `filesystem`.
    /// Nothing is copied: entries keep referring to their source image until edited.
    pub fn create(
        output: &Path,
        tree: PartitionTree,
        filesystem: Filesystem,
    ) -> Result<Self, Error> {
        let mut tree = tree.map(|_, _, metadata| Ok::<_, Error>(EntryMetadata::Set(metadata)))?;
        tree.filesystem = filesystem;
        let content = output.join("content");
        fs::create_dir_all(&content).at(&content)?;
        Ok(Self {
            root: output.into(),
            tree,
            next_blob: 0,
            open: BTreeMap::new(),
        })
    }

    /// The file at `path`, if it is open.
    pub(crate) fn opened(&mut self, path: &RelativePath) -> Result<Option<&mut Open>, Error> {
        let canonical = self.file(path)?.path.clone();
        Ok(self.open.get_mut(&canonical))
    }

    /// The file at `path`, if it is open, to read.
    pub(crate) fn opened_ref(&self, path: &RelativePath) -> Result<Option<&Open>, Error> {
        Ok(self.open.get(self.file(path)?.path))
    }

    /// Open the file at `path` as `open`, which holds its content from now on.
    pub(crate) fn open(&mut self, path: &RelativePath, open: Open) -> Result<(), Error> {
        let canonical = self.file(path)?.path.clone();
        self.open.insert(canonical, open);
        Ok(())
    }

    /// Every open file, by its path.
    pub(crate) fn open_files(&self) -> impl Iterator<Item = (&RelativePath, &Open)> {
        self.open.iter()
    }

    /// Write every file open at or under `path`, or every one, into its file, and
    /// close it.
    pub(crate) fn close(&mut self, path: Option<&RelativePath>) -> Result<(), Error> {
        let open: Vec<RelativePath> = self
            .open
            .keys()
            .filter(|open| path.is_none_or(|path| within(open, path)))
            .cloned()
            .collect();
        for canonical in open {
            let kind = match self.open.remove(&canonical).expect("listed above") {
                // A package goes into its blob as it is written, never whole in
                // memory.
                Open::Package(package) => {
                    let blob = self.new_blob();
                    let path = self.blob_path(&blob);
                    let file = fs::File::create_new(&path).at(&path)?;
                    let size = package.render(io::BufWriter::new(file))?;
                    let content = Content::Blob(blob);
                    EntryKind::File { content, size }
                }
                open => self.write_blob(open.render()?.as_slice())?,
            };
            self.set_content(&canonical, kind);
        }
        Ok(())
    }

    /// Drop every file open at or under `path`: what is written there replaces it.
    fn discard(&mut self, path: &RelativePath) {
        self.open.retain(|open, _| !within(open, path));
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
        let canonical = self.file(path)?.path.clone();
        self.close(Some(&canonical))?;
        let file = self.file(path)?;
        let content = match file.content {
            Content::Blob(blob) => return Ok(self.blob_path(blob)),
            content => content.clone(),
        };
        let (canonical, size) = (file.path.clone(), file.size);
        let blob = self.new_blob();
        let path = self.blob_path(&blob);
        let mut output = fs::File::create_new(&path).at(&path)?;
        io::copy(&mut self.store(images).open(&content, size)?, &mut output).at(&path)?;
        if let Some(entry) = self.tree.entries.get_mut(&canonical) {
            entry.kind = EntryKind::File {
                content: Content::Blob(blob.clone()),
                size,
            };
        }
        Ok(self.blob_path(&blob))
    }

    /// Give a file (and all of its hardlinks) new content.
    pub(crate) fn write(&mut self, path: &RelativePath, bytes: &[u8]) -> Result<(), Error> {
        let canonical = self.file(path)?.path.clone();
        self.discard(&canonical);
        let kind = self.write_blob(bytes)?;
        self.set_content(&canonical, kind);
        Ok(())
    }

    /// Copy a subtree to another path in this target, with the metadata the copy
    /// states.
    pub(crate) fn copy_within(
        &mut self,
        from: &RelativePath,
        to: &RelativePath,
        placing: Placing,
    ) -> Result<(), Error> {
        self.close(Some(from))?;
        self.discard(to);
        let selected = layout::subtree(&self.tree, from, to)?;
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
        &mut self,
        destination: &mut Self,
        from: &RelativePath,
        to: &RelativePath,
        placing: Placing,
    ) -> Result<(), Error> {
        self.close(Some(from))?;
        destination.discard(to);
        let selected = layout::subtree(&self.tree, from, to)?;
        let mut written = destination.destination(to, placing, |dir| {
            layout::directory(&self.tree, dir).cloned()
        })?;
        written.extend(selected.keys().cloned());
        let adopted = destination.adopt(&self.root, selected)?;
        destination.tree.entries.extend(adopted);
        destination.give(written, placing.metadata);
        Ok(())
    }

    /// Give `canonical`, and so all of its hardlinks, new content.
    fn set_content(&mut self, canonical: &RelativePath, kind: EntryKind) {
        if let Some(entry) = self.tree.entries.get_mut(canonical) {
            entry.kind = kind;
        }
    }

    /// A regular file, following a hardlink to its canonical entry.
    pub(crate) fn file(&self, path: &RelativePath) -> Result<File<'_>, Error> {
        Ok(self.tree.file(path)?)
    }

    /// The content of the file at `path`: an open file's, rendered.
    pub(crate) fn read(&self, path: &RelativePath, images: &Images) -> Result<Vec<u8>, Error> {
        let file = self.file(path)?;
        match self.open.get(file.path) {
            Some(open) => open.render(),
            None => Ok(self.store(images).read(&file)?),
        }
    }

    /// The SHA-256 of the file at `path`'s content.
    pub(crate) fn sha256(
        &self,
        path: &RelativePath,
        images: &Images,
    ) -> Result<android::hash::Sha256Digest, Error> {
        let file = self.file(path)?;
        match self.open.contains_key(file.path) {
            true => Ok(android::hash::Sha256Digest::bytes(
                &self.read(path, images)?,
            )),
            false => Ok(self.store(images).sha256(&file)?),
        }
    }

    /// Write a new blob from `reader`.
    fn write_blob(&mut self, mut reader: impl Read) -> Result<EntryKind, Error> {
        let blob = self.new_blob();
        let path = self.blob_path(&blob);
        let size = io::copy(&mut reader, &mut fs::File::create_new(&path).at(&path)?).at(&path)?;
        Ok(EntryKind::File {
            content: Content::Blob(blob),
            size,
        })
    }

    /// Bring entries into this target. Image contents are shared as they are; blobs of
    /// the tree at `source_root` are linked (or copied) into this target's store.
    fn adopt(
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
                    fs::copy(&source, &destination).at(&source)?;
                }
            }
        }
        Ok(entries)
    }

    fn new_blob(&mut self) -> String {
        self.next_blob += 1;
        format!("t{}", self.next_blob)
    }

    fn blob_path(&self, blob: &str) -> PathBuf {
        self.root.join(PartitionTree::blob_path(blob).as_ref())
    }

    /// Remove `path` (or every entry a glob matches) and its contents.
    pub(crate) fn remove(&mut self, path: &RelativePath) -> Result<(), Error> {
        for path in layout::matches(&self.tree, path) {
            self.discard(&path);
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
            self.discard(&to_path);
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
        self.discard(to);
        let made = self.destination(to, placing, |_| None)?;
        let kind = self.write_blob(std::fs::File::open(file).at(file)?)?;
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
    fn reset(&mut self, path: &RelativePath) {
        if let Some(entry) = self.tree.entries.get_mut(path) {
            entry.metadata = EntryMetadata::Default(Vec::new());
        }
    }

    /// The finished tree, its documents written back: every entry waiting for
    /// them gets Android's defaults for where it is on the device, from
    /// `defaults`, then what was set over them.
    pub(crate) fn resolve(
        self,
        partition: &str,
        mounts: &android::partition::Mounts,
        defaults: Option<&crate::patch::defaults::Defaults>,
    ) -> Result<PartitionTree, Error> {
        self.tree.map(|path, kind, metadata| match metadata {
            EntryMetadata::Set(metadata) => Ok(metadata),
            EntryMetadata::Default(overrides) => {
                let at = android::partition::PartitionPath {
                    partition: partition.to_owned(),
                    path: path.clone(),
                };
                let device = mounts
                    .device_path(&at)
                    .ok_or_else(|| Error::UnknownPartition(partition.to_owned()))?;
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

    /// Clear `path` for a new entry: an entry there is removed only to `overwrite`
    /// it, and missing parent directories are made, with the metadata `like`
    /// gives them, only to `create_parent`; the directories made.
    fn destination(
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
    fn give(&mut self, written: BTreeSet<RelativePath>, metadata: Metadata) {
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

/// The metadata `SetMetadata` sets; what it leaves unset stays.
#[derive(Clone, Debug, Default)]
pub(crate) struct Overrides {
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub mode: Option<u16>,
    pub selinux: Option<String>,
}

/// Set what `overrides` sets on `metadata`.
fn set(metadata: &mut AndroidMetadata, overrides: &Overrides) {
    metadata.uid = overrides.uid.unwrap_or(metadata.uid);
    metadata.gid = overrides.gid.unwrap_or(metadata.gid);
    metadata.mode = overrides.mode.unwrap_or(metadata.mode);
    if let Some(label) = &overrides.selinux {
        metadata.set_selinux(label);
    }
}

/// Whether `entry` is `path`, or below it.
fn within(entry: &RelativePath, path: &RelativePath) -> bool {
    let (entry, path) = (entry.as_ref(), path.as_ref());
    entry == path
        || entry
            .strip_prefix(path)
            .is_some_and(|rest| rest.starts_with('/'))
}
