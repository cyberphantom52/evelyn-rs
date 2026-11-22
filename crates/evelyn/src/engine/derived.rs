//! Files the build ships that are derived from others, made again where an edit
//! made them stale: once the recipes are applied and the workspace has written
//! its documents back, and through the workspace, before its trees are finished.
//! Only those the build already has are made. A file changed when its content is not the one its partition's
//! source had at its path, whatever edit changed it.
//!
//! - An odex (`oat/<isa>/<name>.odex`, with its vdex and app image) is stale when
//!   ART would reject it: when the DEX file it is compiled from changed, or a jar
//!   its class loader context records did. It is compiled again (see
//!   [`crate::engine::dexopt`]). A changed boot class path jar makes the boot
//!   image stale, which is not made again: the build stops.
//! - A `.fsv_meta`, a file's fs-verity metadata, is stale when the file changed.

use crate::Firmware;
use crate::engine::dexopt::{self, Art, Job};
use crate::engine::{Error, IoAt, device::Device, device::Workspace, device::relative};
use crate::patch::{Images, Sources};
use crate::toolchain::Tools;
use android::art::Oat;
use android::partition::{Mounts, PartitionPath};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

/// How much of an odex is read for its oat header: past its ELF headers,
/// dynamic tables and the header itself.
const HEADER: u64 = 1 << 18;

pub(crate) fn refresh(
    targets: &mut Workspace,
    origins: &BTreeMap<String, Firmware>,
    sources: &Sources,
    images: &Images,
    mounts: &Mounts,
    tools: &Tools,
    work: &Path,
) -> Result<(), Error> {
    if work.exists() {
        std::fs::remove_dir_all(work).at(work)?;
    }
    let mut written = Vec::new();
    {
        let mut device = Device::new(
            targets,
            images,
            mounts,
            sources,
            origins.clone(),
            &work.join("apex"),
        );
        let jobs = stale_odexes(&mut device)?;
        // Without qemu-user the stale odex files stay as they are, which ART
        // ignores, as it would have without this stage.
        let qemu = match jobs.is_empty() {
            true => None,
            false => match tools.host(&["qemu-aarch64-static", "qemu-aarch64"]) {
                Ok(qemu) => Some(qemu),
                Err(error) => {
                    tracing::warn!(stale = jobs.len(), reason = %error, "odex files left stale, which ART will not use");
                    None
                }
            },
        };
        if let Some(qemu) = qemu {
            let art = Art::new(&mut device, qemu, work)?;
            for job in &jobs {
                tracing::info!(dex = %job.dex, isa = %job.isa, "compiling ART artifacts again");
                let files = art
                    .compile(job, &mut device)
                    .map_err(|source| Error::Dexopt {
                        dex: job.dex.clone(),
                        source: Box::new(source),
                    })?;
                for (at, file) in files {
                    written.push((at, std::fs::read(&file).at(&file)?));
                }
            }
        }
    }
    for (at, data) in written {
        targets
            .get_mut(&at.partition)
            .expect("a path in the build")
            .write(&at.path, &data)?;
    }

    let mut metadata = Vec::new();
    for (partition, target) in targets.iter() {
        for path in target.tree.entries.keys() {
            let Some(file) = path.as_ref().strip_suffix(".fsv_meta") else {
                continue;
            };
            let at = PartitionPath {
                partition: partition.clone(),
                path: relative(file.to_owned()),
            };
            if target.file(&at.path).is_err() {
                continue;
            }
            if crate::engine::device::changed(targets, sources, origins, &at) {
                let data = target.read(&at.path, images)?;
                let at = PartitionPath {
                    partition: partition.clone(),
                    path: path.clone(),
                };
                metadata.push((at, android::art::fsverity_metadata(&data)));
            }
        }
    }
    for (at, data) in metadata {
        targets
            .get_mut(&at.partition)
            .expect("a path in the build")
            .write(&at.path, &data)?;
    }
    Ok(())
}

/// Every odex in the build that an edit made stale, with what compiling it again
/// needs.
fn stale_odexes(device: &mut Device<'_>) -> Result<Vec<Job>, Error> {
    let mut changed: BTreeMap<String, bool> = BTreeMap::new();
    let mut is_changed = |device: &mut Device<'_>, path: &str| -> Result<bool, Error> {
        if let Some(changed) = changed.get(path) {
            return Ok(*changed);
        }
        let result = device.changed(path)?;
        changed.insert(path.to_owned(), result);
        Ok(result)
    };
    let mut jobs = Vec::new();
    for (artifacts, isa, dex) in odexes(device)? {
        let odex = {
            let target = &device.targets[&artifacts.partition];
            let file = target.file(&relative(format!("{}.odex", artifacts.path)))?;
            let mut start = Vec::new();
            target
                .store(device.images)
                .open(file.content, file.size)?
                .take(HEADER)
                .read_to_end(&mut start)
                .at(file.path.as_ref())?;
            Oat::read(&start)?
        };
        let boot = odex.get("bootclasspath")?.to_owned();
        for jar in boot.split(':') {
            if is_changed(device, jar)? {
                return Err(Error::StaleBootImage(jar.to_owned()));
            }
        }
        let own = is_changed(device, &dex)?;
        let mut stale = own;
        for jar in dexopt::paths(odex.store.get("classpath").map_or("", |s| s)) {
            stale |= is_changed(device, &jar)?;
        }
        if !stale {
            continue;
        }
        let original = match own {
            true => match device.locate(&dex)? {
                Some(at) => device.original(&at)?,
                None => None,
            },
            false => None,
        };
        jobs.push(Job {
            artifacts,
            isa,
            dex,
            original,
        });
    }
    Ok(jobs)
}

/// Every odex in the build, `<dir>/oat/<isa>/<name>.odex`, with its instruction
/// set and the DEX file location it is compiled from: the package beside `oat/`
/// named `<name>` (a JAR or APK), or for a jar in an APEX, which the build
/// compiles outside it, the path its name spells with `@` for `/`
/// (`apex@<apex>@javalib@<jar>@classes`).
fn odexes(device: &Device<'_>) -> Result<Vec<(PartitionPath, String, String)>, Error> {
    let mut found = Vec::new();
    for (partition, target) in device.targets {
        let tree = &target.tree;
        for path in tree.entries.keys() {
            let Some(base) = path.as_ref().strip_suffix(".odex") else {
                continue;
            };
            let mut parts = base.rsplitn(4, '/');
            let (Some(name), Some(isa), Some("oat")) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let dir = parts
                .next()
                .map(|dir| format!("{dir}/"))
                .unwrap_or_default();
            let dex = match name.strip_suffix("@classes") {
                Some(apex) => apex.replace('@', "/").replacen("apex/", "/apex/", 1),
                None => {
                    let package = ["jar", "apk"].iter().find_map(|extension| {
                        let path = relative(format!("{dir}{name}.{extension}"));
                        tree.entries.contains_key(&path).then_some(path)
                    });
                    let device_path = package.and_then(|path| {
                        device.mounts.device_path(&PartitionPath {
                            partition: partition.clone(),
                            path,
                        })
                    });
                    match device_path {
                        Some(path) => path.to_string(),
                        None => continue,
                    }
                }
            };
            let artifacts = PartitionPath {
                partition: partition.clone(),
                path: relative(base.to_owned()),
            };
            found.push((artifacts, isa.to_owned(), dex));
        }
    }
    Ok(found)
}
