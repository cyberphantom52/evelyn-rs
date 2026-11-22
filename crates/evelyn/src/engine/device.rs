//! The composed build, its documents written back, as the device sees it: files by device path, links
//! followed, and `/apex/<name>/…` read from the payload of the APEX the build
//! ships under that name. Whether a file changed is whether its content is the
//! one the partition's source had at the same path.

use crate::engine::{Error, IoAt};
use crate::patch::Target;
use crate::patch::{Images, Sources};
use crate::{Firmware, SourceRef};
use android::partition::{Mounts, PartitionPath};
use android::path::{DevicePath, RelativePath};
use android::{EntryKind, Image, PartitionTree, Store};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The build's partitions, by name, as the recipes left them.
pub(crate) type Workspace = BTreeMap<String, Target>;

pub(crate) struct Device<'a> {
    pub targets: &'a Workspace,
    pub images: &'a Images,
    pub mounts: &'a Mounts,
    sources: &'a Sources,
    origins: BTreeMap<String, Firmware>,
    /// Where APEX payloads are written to be opened.
    work: PathBuf,
    /// The build's APEX files by the name their manifest gives them, once read.
    apexes: Option<BTreeMap<String, PartitionPath>>,
    payloads: BTreeMap<String, Image>,
}

impl<'a> Device<'a> {
    pub fn new(
        targets: &'a Workspace,
        images: &'a Images,
        mounts: &'a Mounts,
        sources: &'a Sources,
        origins: BTreeMap<String, Firmware>,
        work: &Path,
    ) -> Self {
        Self {
            targets,
            images,
            mounts,
            sources,
            origins,
            work: work.to_path_buf(),
            apexes: None,
            payloads: BTreeMap::new(),
        }
    }

    /// Where the file at `path` is in a partition, links followed; not for
    /// `/apex`.
    pub fn locate(&self, path: &str) -> Result<Option<PartitionPath>, Error> {
        let device = DevicePath::try_from(path.to_owned())?;
        for at in self.mounts.resolve(&device) {
            let Some(target) = self.targets.get(&at.partition) else {
                continue;
            };
            match target.tree.entries.get(&at.path).map(|entry| &entry.kind) {
                Some(EntryKind::Symlink { target }) => {
                    return self.locate(&linked(path, &String::from_utf8_lossy(target)));
                }
                Some(EntryKind::File { .. } | EntryKind::Hardlink { .. }) => return Ok(Some(at)),
                _ => {}
            }
        }
        Ok(None)
    }

    /// The content of the file at `path`, if the build has it.
    pub fn read(&mut self, path: &str) -> Result<Option<Vec<u8>>, Error> {
        if let Some((name, file)) = apex_path(path) {
            let image = self.payload(name)?;
            return Ok(image
                .read_files(|path| path == file)?
                .pop()
                .map(|(_, data)| data));
        }
        let Some(at) = self.locate(path)? else {
            return Ok(None);
        };
        Ok(Some(self.read_at(&at)?))
    }

    pub fn read_at(&self, at: &PartitionPath) -> Result<Vec<u8>, Error> {
        Ok(self.targets[&at.partition].read(&at.path, self.images)?)
    }

    /// Every file of the payload of the APEX `name`, by its path in the APEX.
    pub fn apex(&mut self, name: &str) -> Result<Vec<(String, Vec<u8>)>, Error> {
        Ok(self.payload(name)?.read_files(|_| true)?)
    }

    /// The names of the entries directly in the directory `dir`.
    pub fn list(&self, dir: &str) -> Result<Vec<String>, Error> {
        let device = DevicePath::try_from(dir.to_owned())?;
        let mut names = Vec::new();
        for at in self.mounts.resolve(&device) {
            let Some(target) = self.targets.get(&at.partition) else {
                continue;
            };
            let prefix = format!("{}/", at.path);
            names.extend(
                target
                    .tree
                    .entries
                    .keys()
                    .filter_map(|path| path.as_ref().strip_prefix(&prefix))
                    .filter(|name| !name.contains('/'))
                    .map(str::to_owned),
            );
        }
        Ok(names)
    }

    /// Whether the file at `path` is not what its partition's source had there:
    /// different, new, or gone; a file in an APEX changed if its APEX did.
    pub fn changed(&mut self, path: &str) -> Result<bool, Error> {
        if let Some((name, _)) = apex_path(path) {
            return Ok(match self.apex_file(name)? {
                Some(at) => changed(self.targets, self.sources, &self.origins, &at),
                None => false,
            });
        }
        Ok(match self.locate(path)? {
            Some(at) => changed(self.targets, self.sources, &self.origins, &at),
            // Gone from the build, or never in it or its sources.
            None => self
                .mounts
                .resolve(&DevicePath::try_from(path.to_owned())?)
                .iter()
                .any(|at| {
                    self.source(&at.partition)
                        .is_some_and(|source| source.entries.contains_key(&at.path))
                }),
        })
    }

    /// The content the partition's source had at `at`.
    pub fn original(&self, at: &PartitionPath) -> Result<Option<Vec<u8>>, Error> {
        let Some(source) = self.source(&at.partition) else {
            return Ok(None);
        };
        match source.file(&at.path) {
            Ok(file) => Ok(Some(Store::sources(self.images).read(&file)?)),
            Err(_) => Ok(None),
        }
    }

    fn source(&self, partition: &str) -> Option<&'a PartitionTree> {
        source(self.sources, &self.origins, partition)
    }

    /// The build's APEX file named `name`, if it has one.
    fn apex_file(&mut self, name: &str) -> Result<Option<PartitionPath>, Error> {
        if self.apexes.is_none() {
            let mut found = BTreeMap::new();
            for (partition, target) in self.targets {
                for (path, entry) in &target.tree.entries {
                    let in_apex_dir = path
                        .as_ref()
                        .rsplit_once('/')
                        .is_some_and(|(dir, _)| dir == "apex" || dir.ends_with("/apex"));
                    let apex =
                        path.as_ref().ends_with(".apex") || path.as_ref().ends_with(".capex");
                    if !in_apex_dir || !apex || !matches!(entry.kind, EntryKind::File { .. }) {
                        continue;
                    }
                    let data = target.read(path, self.images)?;
                    found
                        .entry(android::apex::name(&data)?)
                        .or_insert(PartitionPath {
                            partition: partition.clone(),
                            path: path.clone(),
                        });
                }
            }
            self.apexes = Some(found);
        }
        Ok(self
            .apexes
            .as_ref()
            .and_then(|apexes| apexes.get(name))
            .cloned())
    }

    fn payload(&mut self, name: &str) -> Result<&Image, Error> {
        if !self.payloads.contains_key(name) {
            let at = self
                .apex_file(name)?
                .ok_or_else(|| Error::NoApex(name.to_owned()))?;
            let payload = android::apex::payload(&self.read_at(&at)?)?;
            let path = self.work.join(format!("{name}.img"));
            std::fs::create_dir_all(&self.work).at(&self.work)?;
            std::fs::write(&path, payload).at(&path)?;
            self.payloads.insert(name.to_owned(), Image::detect(&path)?);
        }
        Ok(&self.payloads[name])
    }
}

/// Whether the file at `at` differs from what its partition's source has there.
pub(crate) fn changed(
    targets: &Workspace,
    sources: &Sources,
    origins: &BTreeMap<String, Firmware>,
    at: &PartitionPath,
) -> bool {
    let Ok(file) = targets[&at.partition].file(&at.path) else {
        return true;
    };
    let Some(source) = source(sources, origins, &at.partition) else {
        return true;
    };
    source
        .file(&at.path)
        .map_or(true, |original| original.content != file.content)
}

fn source<'a>(
    sources: &'a Sources,
    origins: &BTreeMap<String, Firmware>,
    partition: &str,
) -> Option<&'a PartitionTree> {
    sources.get(&SourceRef {
        source: *origins.get(partition)?,
        partition: partition.to_owned(),
    })
}

/// The APEX name and the path in it of a path under `/apex/`.
fn apex_path(path: &str) -> Option<(&str, &str)> {
    path.strip_prefix("/apex/")?.split_once('/')
}

/// The device path the link at `path` to `target` leads to.
fn linked(path: &str, target: &str) -> String {
    let mut components: Vec<&str> = match target.starts_with('/') {
        true => Vec::new(),
        false => path.split('/').filter(|c| !c.is_empty()).collect(),
    };
    if !target.starts_with('/') {
        components.pop();
    }
    for component in target.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop();
            }
            component => components.push(component),
        }
    }
    format!("/{}", components.join("/"))
}

/// A relative path from its text, for a path made from the build's own.
pub(crate) fn relative(path: String) -> RelativePath {
    RelativePath::try_from(path).expect("made of the build's own paths")
}
