//! The firmware a build reads: a directory of `<partition>.img` images, or a full
//! OTA package, a zip holding `payload.bin` or `payload.bin` itself, whose
//! partitions are extracted once into a directory of images and checked against
//! the hashes its manifest records.

use crate::Error;
use android_firmware::Sha256Digest;
use payload_dumper::{
    AsyncPayloadRead, DeltaArchiveManifest, FileType, LocalAsyncPayloadReader,
    LocalAsyncZipPayloadReader, NoOpReporter,
};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

/// What extraction records of the package it extracted, to extract it again only
/// when the package changes.
const EXTRACTED: &str = ".extracted";

/// The directory of images `given` is: `given` itself, or the images of the OTA
/// package it is or holds, extracted below `cache`. An OTA's dynamic partitions
/// are extracted, and with `firmware`, its other partitions too, into `firmware/`,
/// for a flashable ZIP to flash.
pub(crate) fn images(given: &Path, cache: &Path, firmware: bool) -> Result<PathBuf, Error> {
    let Some(package) = package(given)? else {
        return Ok(given.to_path_buf());
    };
    let metadata = fs::metadata(&package)?;
    let modified = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let record = format!(
        "{}\n{}\n{}.{}\n{firmware}\n",
        fs::canonicalize(&package)?.display(),
        metadata.len(),
        modified.as_secs(),
        modified.subsec_nanos()
    );
    if fs::read_to_string(cache.join(EXTRACTED)).is_ok_and(|extracted| extracted == record) {
        return Ok(cache.to_path_buf());
    }
    if cache.exists() {
        fs::remove_dir_all(cache)?;
    }
    fs::create_dir_all(cache.join("firmware"))?;
    tracing::info!(package = %package.display(), "extracting OTA package");
    extract(&package, cache, firmware)?;
    fs::write(cache.join(EXTRACTED), record)?;
    Ok(cache.to_path_buf())
}

/// The OTA package `given` is or holds, if it is not a directory of images.
fn package(given: &Path) -> Result<Option<PathBuf>, Error> {
    if !given.is_dir() {
        return Ok(Some(given.to_path_buf()));
    }
    let mut packages = Vec::new();
    for entry in fs::read_dir(given)? {
        let path = entry?.path();
        if path.extension().is_some_and(|extension| extension == "img") {
            return Ok(None);
        }
        if path.extension().is_some_and(|extension| extension == "zip")
            || path.file_name().is_some_and(|name| name == "payload.bin")
        {
            packages.push(path);
        }
    }
    match packages.len() {
        0 => Ok(None),
        1 => Ok(packages.pop()),
        _ => Err(Error::AmbiguousFirmware(given.to_path_buf())),
    }
}

fn extract(package: &Path, into: &Path, firmware: bool) -> Result<(), Error> {
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
    for (image, hash) in extracted? {
        let Some(hash) = hash else { continue };
        if Sha256Digest::reader(fs::File::open(&image)?)?.as_bytes()[..] != hash[..] {
            return Err(Error::OtaHash {
                package: package.to_path_buf(),
                image,
            });
        }
    }
    Ok(())
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
