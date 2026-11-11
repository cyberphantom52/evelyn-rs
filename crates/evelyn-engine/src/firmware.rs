//! The firmware a build reads: a directory of `<partition>.img` images, or a full
//! OTA package, a zip holding `payload.bin` or `payload.bin` itself, local or at
//! an `http(s)://` URL. A package's partitions are extracted only as the build
//! needs them, into scratch space removed after it, reading only those
//! partitions' bytes of a remote one, and each is checked against the hash its
//! manifest records. `evelyn extract` replaces a local package by all its
//! images, for good.

use crate::Error;
use android_firmware::{Name, Sha256Digest};
use android_fs::ImageFormat;
use payload_dumper::{
    AsyncPayloadRead, DeltaArchiveManifest, ExtractionPaths, FileType, HttpReader,
    LocalAsyncPayloadReader, LocalAsyncZipPayloadReader, NoOpDownloadReporter, NoOpReporter,
    PartitionExtractionConfig, PartitionUpdate, RemoteAsyncBinPayloadReader,
    RemoteAsyncZipPayloadReader,
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

/// Where a firmware is: a local file or directory, or a URL.
#[derive(Clone, Debug)]
pub enum Given {
    Path(PathBuf),
    Url(String),
}

impl Given {
    /// A URL if `text` is an `http(s)://` one, else a path.
    pub fn parse(text: &str) -> Self {
        match text.starts_with("http://") || text.starts_with("https://") {
            true => Self::Url(text.to_owned()),
            false => Self::Path(PathBuf::from(text)),
        }
    }
}

impl std::fmt::Display for Given {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Path(path) => write!(f, "{}", path.display()),
            Self::Url(url) => f.write_str(url),
        }
    }
}

/// A firmware's partitions, each image there once found or extracted.
pub(crate) struct Firmware {
    /// Every filesystem partition it has, with its image once there.
    images: BTreeMap<Name, Option<PathBuf>>,
    /// Where a directory of images keeps those a flashable ZIP flashes.
    firmware_dir: Option<PathBuf>,
    package: Option<Package>,
}

/// An OTA package, open for extracting partitions from.
struct Package {
    given: Given,
    runtime: tokio::runtime::Runtime,
    manifest: DeltaArchiveManifest,
    offset: u64,
    reader: Arc<dyn AsyncPayloadRead>,
    remote: Option<Remote>,
    /// The partitions outside `super`, which a flashable ZIP flashes.
    other: Vec<String>,
    scratch: PathBuf,
}

/// A package at a URL: each partition's data is downloaded in one request, then
/// extracted from that, rather than read an operation at a time.
struct Remote {
    http: Arc<HttpReader>,
    /// Where the payload starts in what the URL serves.
    payload_offset: u64,
}

impl Drop for Package {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.scratch);
    }
}

impl Firmware {
    /// Open the firmware `given` is or holds, extracting a package's partitions
    /// into `scratch` as they are asked for.
    pub(crate) fn open(given: &Given, scratch: &Path) -> Result<Self, Error> {
        let given = match given {
            Given::Url(_) => given.clone(),
            Given::Path(path) => match package(path)? {
                Some(package) => Given::Path(package),
                None => {
                    let mut images = BTreeMap::new();
                    for (name, image) in image_files(path)? {
                        if ImageFormat::detect(&image)? != ImageFormat::Unknown {
                            images.insert(name, Some(image));
                        }
                    }
                    return Ok(Self {
                        images,
                        firmware_dir: Some(path.join("firmware")),
                        package: None,
                    });
                }
            },
        };
        if scratch.exists() {
            fs::remove_dir_all(scratch)?;
        }
        fs::create_dir_all(scratch)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let (manifest, offset, reader, remote) = runtime.block_on(open_payload(&given))?;
        if manifest.minor_version.is_some_and(|version| version != 0) {
            return Err(Error::IncrementalOta(given.to_string()));
        }
        // Without dynamic partitions, every partition is one the build may take.
        let dynamic: Option<BTreeSet<String>> =
            manifest
                .dynamic_partition_metadata
                .as_ref()
                .map(|metadata| {
                    metadata
                        .groups
                        .iter()
                        .flat_map(|group| group.partition_names.iter().cloned())
                        .collect()
                });
        let mut images = BTreeMap::new();
        let mut other = Vec::new();
        for partition in &manifest.partitions {
            let name = &partition.partition_name;
            match &dynamic {
                Some(dynamic) if !dynamic.contains(name) => other.push(name.clone()),
                _ => {
                    if let Ok(name) = Name::try_from(name.clone()) {
                        images.insert(name, None);
                    }
                }
            }
        }
        let mut firmware = Self {
            images,
            firmware_dir: None,
            package: Some(Package {
                given,
                runtime,
                manifest,
                offset,
                reader,
                remote,
                other,
                scratch: scratch.to_path_buf(),
            }),
        };
        if dynamic.is_none() {
            // Which partitions are filesystems is only known from their images.
            let all: Vec<Name> = firmware.images.keys().cloned().collect();
            firmware.extract(&all)?;
            for name in all {
                let image = firmware.images[&name].clone().expect("extracted");
                if ImageFormat::detect(&image)? == ImageFormat::Unknown {
                    firmware.images.remove(&name);
                }
            }
        }
        Ok(firmware)
    }

    /// Every filesystem partition the firmware has.
    pub(crate) fn names(&self) -> impl Iterator<Item = &Name> {
        self.images.keys()
    }

    pub(crate) fn has(&self, name: &str) -> bool {
        self.images.keys().any(|key| key.as_ref() == name)
    }

    /// The image of `name`, once [`Firmware::extract`] has it there.
    pub(crate) fn image(&self, name: &Name) -> Option<&Path> {
        self.images.get(name)?.as_deref()
    }

    /// Have the images of `names` there, extracting those of a package that are
    /// not yet; names the firmware lacks are left out.
    pub(crate) fn extract(&mut self, names: &[Name]) -> Result<(), Error> {
        let Some(package) = &self.package else {
            return Ok(());
        };
        let wanted: Vec<&str> = names
            .iter()
            .filter(|name| matches!(self.images.get(*name), Some(None)))
            .map(|name| name.as_ref())
            .collect();
        for (name, image) in package.dump(&wanted, &package.scratch)? {
            if let Ok(name) = Name::try_from(name) {
                self.images.insert(name, Some(image));
            }
        }
        Ok(())
    }

    /// The images of the partitions outside `super`, which a flashable ZIP
    /// flashes: a directory's `firmware/`, or a package's, extracted.
    pub(crate) fn firmware_images(&self) -> Result<Vec<PathBuf>, Error> {
        if let Some(package) = &self.package {
            let other: Vec<&str> = package.other.iter().map(String::as_str).collect();
            let into = package.scratch.join("firmware");
            fs::create_dir_all(&into)?;
            return Ok(package
                .dump(&other, &into)?
                .into_iter()
                .map(|(_, image)| image)
                .collect());
        }
        let directory = self.firmware_dir.clone().unwrap_or_default();
        if !directory.is_dir() {
            return Err(Error::MissingFirmware(directory));
        }
        Ok(image_files(&directory)?.into_values().collect())
    }
}

impl Package {
    /// Extract the partitions named `names` into `into`, each on a task of its
    /// own, and check each against the hash the manifest records: each name with
    /// its image.
    fn dump(&self, names: &[&str], into: &Path) -> Result<Vec<(String, PathBuf)>, Error> {
        let partitions: Vec<&PartitionUpdate> = self
            .manifest
            .partitions
            .iter()
            .filter(|partition| names.contains(&partition.partition_name.as_str()))
            .collect();
        if partitions.is_empty() {
            return Ok(Vec::new());
        }
        tracing::info!(
            package = %self.given,
            partitions = %names.join(" "),
            "extracting from OTA package"
        );
        let block_size = u64::from(self.manifest.block_size.unwrap_or(4096));
        let extracted = self.runtime.block_on(async {
            let mut tasks = Vec::new();
            for partition in partitions {
                let image = into.join(format!("{}.img", partition.partition_name));
                let hash = partition
                    .new_partition_info
                    .as_ref()
                    .and_then(|info| info.hash.clone());
                let (partition, reader, written) =
                    (partition.clone(), self.reader.clone(), image.clone());
                let offset = self.offset;
                // A partition with data to download is prefetched; one without
                // any is extracted as it is.
                let remote = self.remote.as_ref().filter(|_| {
                    payload_dumper::calculate_partition_range(&partition, offset).is_some()
                });
                let remote = remote.map(|remote| (remote.http.clone(), remote.payload_offset));
                let download = into.join(format!("{}.download", partition.partition_name));
                tasks.push((
                    partition.partition_name.clone(),
                    image,
                    hash,
                    tokio::spawn(async move {
                        let Some((http, payload_offset)) = remote else {
                            return payload_dumper::dump_partition(
                                &partition,
                                offset,
                                block_size,
                                written,
                                &reader,
                                &NoOpReporter,
                                None,
                            )
                            .await;
                        };
                        let dumped = payload_dumper::prefetch_and_dump_partition(
                            &partition,
                            &PartitionExtractionConfig {
                                data_offset: offset,
                                block_size,
                                payload_offset,
                            },
                            &http,
                            ExtractionPaths {
                                temp_path: download.clone(),
                                output_path: written,
                            },
                            &NoOpDownloadReporter,
                            &NoOpReporter,
                            None,
                        )
                        .await;
                        let _ = tokio::fs::remove_file(&download).await;
                        dumped
                    }),
                ));
            }
            let mut extracted = Vec::new();
            for (name, image, hash, task) in tasks {
                task.await.map_err(std::io::Error::other)??;
                extracted.push((name, image, hash));
            }
            Ok::<_, Error>(extracted)
        })?;
        let mut images = Vec::new();
        for (name, image, hash) in extracted {
            if let Some(hash) = hash
                && Sha256Digest::reader(fs::File::open(&image)?)?.as_bytes()[..] != hash[..]
            {
                return Err(Error::OtaHash {
                    package: self.given.to_string(),
                    image,
                });
            }
            images.push((name, image));
        }
        Ok(images)
    }
}

/// What opening a payload gives: its manifest, where its data starts, a reader
/// of it, and for one at a URL, what downloads its data.
type Opened = (
    DeltaArchiveManifest,
    u64,
    Arc<dyn AsyncPayloadRead>,
    Option<Remote>,
);

/// Open the payload `given` is or holds.
async fn open_payload(given: &Given) -> Result<Opened, Error> {
    let mut magic = [0; 4];
    match given {
        Given::Path(path) => {
            fs::File::open(path)?.read_exact(&mut magic)?;
            let path = path.clone();
            Ok(match payload_dumper::detect_file(&magic)? {
                FileType::Zip => {
                    let (manifest, offset, _) =
                        payload_dumper::parse_local_zip_payload(path.clone()).await?;
                    let reader = LocalAsyncZipPayloadReader::new(path).await?;
                    (manifest, offset, Arc::new(reader), None)
                }
                FileType::Bin => {
                    let (manifest, offset) = payload_dumper::parse_local_payload(&path).await?;
                    let reader = LocalAsyncPayloadReader::new(path).await?;
                    (manifest, offset, Arc::new(reader), None)
                }
            })
        }
        Given::Url(url) => {
            let http = Arc::new(HttpReader::new(url.clone(), None, None, None).await?);
            http.read_at(0, &mut magic).await?;
            Ok(match payload_dumper::detect_file(&magic)? {
                FileType::Zip => {
                    let (manifest, offset, zip) =
                        payload_dumper::parse_remote_payload(url.clone(), None, None, None).await?;
                    let reader =
                        RemoteAsyncZipPayloadReader::new(url.clone(), None, None, None).await?;
                    let remote = Remote {
                        http,
                        payload_offset: zip.payload_data_offset,
                    };
                    (manifest, offset, Arc::new(reader), Some(remote))
                }
                FileType::Bin => {
                    let (manifest, offset, _) =
                        payload_dumper::parse_remote_bin_payload(url.clone(), None, None, None)
                            .await?;
                    let reader =
                        RemoteAsyncBinPayloadReader::new(url.clone(), None, None, None).await?;
                    let remote = Remote {
                        http,
                        payload_offset: 0,
                    };
                    (manifest, offset, Arc::new(reader), Some(remote))
                }
            })
        }
    }
}

/// An OTA package replaced by its images in the directory holding it.
#[derive(Debug, Serialize)]
pub struct Extracted {
    pub package: PathBuf,
    pub images: Vec<PathBuf>,
}

/// Replace the OTA package in `dir` by its images, beside where it was, once
/// they are all extracted and checked: with `firmware`, its partitions outside
/// `super` too, into `firmware/`. Any images an earlier extraction left there
/// go first. A directory without a package is left as it is.
pub fn extract_in_place(dir: &Path, firmware: bool) -> Result<Option<Extracted>, Error> {
    let Some(package) = packages(dir)?.pop() else {
        return Ok(None);
    };
    let staging = dir.join(".extracting");
    let opened = Firmware::open(&Given::Path(package.clone()), &staging)?;
    let open = opened.package.as_ref().expect("a package");
    let all: Vec<&str> = open
        .manifest
        .partitions
        .iter()
        .map(|partition| partition.partition_name.as_str())
        .filter(|name| !open.other.iter().any(|other| other == name))
        .collect();
    let mut written: Vec<PathBuf> = open
        .dump(&all, &staging)?
        .into_iter()
        .map(|(_, image)| image)
        .collect();
    if firmware {
        written.extend(opened.firmware_images()?);
    }
    for old in [dir.to_path_buf(), dir.join("firmware")] {
        for image in image_files(&old)?.into_values() {
            fs::remove_file(image)?;
        }
    }
    let mut images = Vec::new();
    for image in written {
        let relative = image.strip_prefix(&staging).unwrap_or(&image);
        let to = dir.join(relative);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&image, &to)?;
        images.push(to);
    }
    drop(opened);
    fs::remove_file(&package)?;
    Ok(Some(Extracted { package, images }))
}

/// The OTA package `given` is or holds, if any. A directory holding none is one
/// of images; one holding a package and images, or two packages, is refused.
fn package(given: &Path) -> Result<Option<PathBuf>, Error> {
    if !given.is_dir() {
        return Ok(Some(given.to_path_buf()));
    }
    let mut packages = packages(given)?;
    if !packages.is_empty() && !image_files(given)?.is_empty() {
        return Err(Error::MixedFirmware(given.to_path_buf()));
    }
    Ok(packages.pop())
}

/// The OTA packages in `dir`; more than one is refused.
fn packages(dir: &Path) -> Result<Vec<PathBuf>, Error> {
    let mut packages = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|extension| extension == "zip")
            || path.file_name().is_some_and(|name| name == "payload.bin")
        {
            packages.push(path);
        }
    }
    if packages.len() > 1 {
        return Err(Error::AmbiguousFirmware(dir.to_path_buf()));
    }
    Ok(packages)
}

/// The `<partition>.img` files directly in `dir`, if it exists.
fn image_files(dir: &Path) -> Result<BTreeMap<Name, PathBuf>, Error> {
    let mut images = BTreeMap::new();
    if !dir.is_dir() {
        return Ok(images);
    }
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(stem) = path
            .extension()
            .is_some_and(|extension| extension == "img")
            .then(|| path.file_stem())
            .flatten()
        else {
            continue;
        };
        if let Ok(name) = Name::try_from(stem.to_string_lossy().into_owned())
            && path.is_file()
        {
            images.insert(name, path);
        }
    }
    Ok(images)
}
