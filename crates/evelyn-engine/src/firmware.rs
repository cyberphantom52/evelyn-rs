//! The firmware a build reads: a directory of `<partition>.img` images, or a full
//! OTA package, a zip holding `payload.bin` or `payload.bin` itself, whose
//! partitions are extracted and checked against the hashes its manifest records:
//! for one build into scratch space, or for good into the directory holding it.

use crate::Error;
use android_firmware::Sha256Digest;
use payload_dumper::{
    AsyncPayloadRead, DeltaArchiveManifest, FileType, LocalAsyncPayloadReader,
    LocalAsyncZipPayloadReader, NoOpReporter,
};
use serde::Serialize;
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

/// A package's images, extracted for one plan and removed with it.
#[derive(Debug)]
pub(crate) struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The directory of images `given` is: `given` itself, or, for an OTA package
/// or a directory holding one, its images extracted into `scratch`, which lives
/// as long as what is returned with it. An OTA's dynamic partitions are
/// extracted, and with `firmware`, its other partitions too, into `firmware/`,
/// for a flashable ZIP to flash.
pub(crate) fn images(
    given: &Path,
    scratch: &Path,
    firmware: bool,
) -> Result<(PathBuf, Option<Scratch>), Error> {
    let Some(package) = package(given)? else {
        return Ok((given.to_path_buf(), None));
    };
    if scratch.exists() {
        fs::remove_dir_all(scratch)?;
    }
    fs::create_dir_all(scratch)?;
    let kept = Scratch(scratch.to_path_buf());
    tracing::info!(package = %package.display(), "extracting OTA package");
    extract(&package, scratch, firmware)?;
    Ok((scratch.to_path_buf(), Some(kept)))
}

/// An OTA package replaced by its images in the directory holding it.
#[derive(Debug, Serialize)]
pub struct Extracted {
    pub package: PathBuf,
    pub images: Vec<PathBuf>,
}

/// Replace the OTA package in `dir` by its images, beside where it was, once
/// they are all extracted and checked: any images an earlier extraction left
/// there go first. A directory without a package is left as it is.
pub fn extract_in_place(dir: &Path, firmware: bool) -> Result<Option<Extracted>, Error> {
    let Some(package) = packages(dir)?.pop() else {
        return Ok(None);
    };
    let staging = dir.join(".extracting");
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir_all(&staging)?;
    tracing::info!(package = %package.display(), "extracting OTA package");
    let written = match extract(&package, &staging, firmware) {
        Ok(written) => written,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    for old in [dir.to_path_buf(), dir.join("firmware")] {
        for image in image_files(&old)? {
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
    fs::remove_dir_all(&staging)?;
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

/// The `.img` files directly in `dir`, if it exists.
fn image_files(dir: &Path) -> Result<Vec<PathBuf>, Error> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut images = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_file() && path.extension().is_some_and(|extension| extension == "img") {
            images.push(path);
        }
    }
    Ok(images)
}

/// Extract `package` into `into`; the images written, each checked.
fn extract(package: &Path, into: &Path, firmware: bool) -> Result<Vec<PathBuf>, Error> {
    if firmware {
        fs::create_dir_all(into.join("firmware"))?;
    }
    let mut magic = [0; 4];
    fs::File::open(package)?.read_exact(&mut magic)?;
    let runtime = tokio::runtime::Builder::new_multi_thread().build()?;
    let path = package.to_path_buf();
    let extracted = runtime.block_on(async {
        match payload_dumper::detect_file(&magic)? {
            FileType::Zip => {
                let (manifest, offset, _) =
                    payload_dumper::parse_local_zip_payload(path.clone()).await?;
                let reader = LocalAsyncZipPayloadReader::new(path).await?;
                dump(manifest, offset, reader, package, into, firmware).await
            }
            FileType::Bin => {
                let (manifest, offset) = payload_dumper::parse_local_payload(&path).await?;
                let reader = LocalAsyncPayloadReader::new(path).await?;
                dump(manifest, offset, reader, package, into, firmware).await
            }
        }
    });
    let extracted = extracted?;
    for (image, hash) in &extracted {
        let Some(hash) = hash else { continue };
        if Sha256Digest::reader(fs::File::open(image)?)?.as_bytes()[..] != hash[..] {
            return Err(Error::OtaHash {
                package: package.to_path_buf(),
                image: image.clone(),
            });
        }
    }
    Ok(extracted.into_iter().map(|(image, _)| image).collect())
}

/// Extract the partitions `manifest` describes, each on a task of its own; the
/// images written, with the hash the manifest records for each.
async fn dump<P: AsyncPayloadRead + 'static>(
    manifest: DeltaArchiveManifest,
    offset: u64,
    reader: P,
    package: &Path,
    into: &Path,
    firmware: bool,
) -> Result<Vec<(PathBuf, Option<Vec<u8>>)>, Error> {
    if manifest.minor_version.is_some_and(|version| version != 0) {
        return Err(Error::IncrementalOta(package.to_path_buf()));
    }
    // Without dynamic partitions, every partition is one the build may take.
    let dynamic: Option<BTreeSet<&str>> =
        manifest
            .dynamic_partition_metadata
            .as_ref()
            .map(|metadata| {
                metadata
                    .groups
                    .iter()
                    .flat_map(|group| &group.partition_names)
                    .map(String::as_str)
                    .collect()
            });
    let block_size = u64::from(manifest.block_size.unwrap_or(4096));
    let reader = Arc::new(reader);
    let mut tasks = Vec::new();
    for partition in &manifest.partitions {
        let name = &partition.partition_name;
        let image = match &dynamic {
            Some(dynamic) if !dynamic.contains(name.as_str()) => match firmware {
                true => into.join("firmware").join(format!("{name}.img")),
                false => continue,
            },
            _ => into.join(format!("{name}.img")),
        };
        let hash = partition
            .new_partition_info
            .as_ref()
            .and_then(|info| info.hash.clone());
        let (partition, reader, written) = (partition.clone(), reader.clone(), image.clone());
        tasks.push((
            image,
            hash,
            tokio::spawn(async move {
                payload_dumper::dump_partition(
                    &partition,
                    offset,
                    block_size,
                    written,
                    &*reader,
                    &NoOpReporter,
                    None,
                )
                .await
            }),
        ));
    }
    let mut extracted = Vec::new();
    for (image, hash, task) in tasks {
        task.await.map_err(std::io::Error::other)??;
        extracted.push((image, hash));
    }
    Ok(extracted)
}
