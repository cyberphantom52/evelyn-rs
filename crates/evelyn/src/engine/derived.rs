//! Files the build ships that are derived from others, made again where an edit
//! made them stale: once the recipes are applied and the workspace has written
//! its documents back, and through the workspace, before its trees are finished.
//! Only those the build already has are made. A file changed when its content is
//! not the one its partition's source had at its path, whatever edit changed it.
//!
//! - The boot image (`/system/framework/<isa>/boot*.{art,oat}` and the vdex
//!   files) is the primary image, of the platform's boot jars, then an extension,
//!   of the APEXes' boot jars, as the boot class path and its checksums an odex
//!   records lay out. An image is stale when a jar it covers changed, or an
//!   image before it is; it is compiled again, with the primary image's profile,
//!   `/system/etc/boot-image.prof`, when one of its jars changed.
//!   `/system/etc/boot-image.bprof`, a profile only boot-time profiling reads,
//!   is left as it is.
//! - An odex (`oat/<isa>/<name>.odex`, with its vdex and app image) is stale when
//!   ART would reject it: when the DEX file it is compiled from changed, a jar
//!   its class loader context records did, or the boot image it was compiled
//!   against is stale. It is compiled again, after the boot image.
//! - A `.fsv_meta`, a file's fs-verity metadata, is stale when the file changed.
//!
//! See [`crate::engine::dexopt`] for how each is compiled.

use crate::Firmware;
use crate::engine::dexopt::{self, Art, BootJob, Job};
use crate::engine::{Error, IoAt, device::Device, device::Workspace, device::relative};
use crate::patch::{Images, Sources};
use crate::toolchain::Tools;
use android::art::{ImageHeader, Oat};
use android::partition::{Mounts, PartitionPath};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

/// How much of an odex is read for its oat header: past its ELF headers,
/// dynamic tables and the header itself.
const HEADER: u64 = 1 << 18;

/// What becomes of the ART files an edit made stale.
pub(crate) enum Dexpreopt {
    /// They are compiled again.
    Compile,
    /// They are counted, for a dry run, which writes nothing.
    Count,
    /// They are left as they are, which ART does not use.
    Skip,
}

/// Make the build's stale derived files again, the ART files as `dexpreopt` says.
#[allow(clippy::too_many_arguments)]
pub(crate) fn refresh(
    targets: &mut Workspace,
    origins: &BTreeMap<String, Firmware>,
    sources: &Sources,
    images: &Images,
    mounts: &Mounts,
    tools: &Tools,
    work: &Path,
    dexpreopt: Dexpreopt,
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
        let (boot, jobs) = stale(&mut device)?;
        let stale = boot.len() + jobs.len();
        match dexpreopt {
            _ if stale == 0 => {}
            Dexpreopt::Compile => {}
            Dexpreopt::Count => tracing::info!(
                boot_images = boot.len(),
                odex = jobs.len(),
                "ART files that a build compiles again"
            ),
            Dexpreopt::Skip => tracing::warn!(
                boot_images = boot.len(),
                odex = jobs.len(),
                "ART files left stale, which ART will not use, as --no-dexpreopt asks"
            ),
        }
        let compile = matches!(dexpreopt, Dexpreopt::Compile);
        // The ROM's tools run as they are on arm64 Linux and Android, and
        // under qemu-user elsewhere; without qemu-user the stale files stay as
        // they are, which ART ignores, as it would have without this stage.
        let native = cfg!(any(target_os = "linux", target_os = "android"))
            && std::env::consts::ARCH == "aarch64";
        let runner = match stale {
            0 => None,
            _ if !compile => None,
            _ if native => Some(None),
            _ => match tools.host(&["qemu-aarch64-static", "qemu-aarch64"]) {
                Ok(qemu) => Some(Some(qemu)),
                Err(error) => {
                    tracing::warn!(stale, reason = %error, "ART files left stale, which ART will not use");
                    None
                }
            },
        };
        if let Some(emulator) = runner {
            let art = Art::new(&mut device, emulator, work)?;
            // Each image's checksum as oat files record it, by instruction set,
            // the ROM's until it is compiled again.
            let mut checksums: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for isa in boot
                .iter()
                .map(|job| &job.isa)
                .chain(jobs.iter().map(|job| &job.isa))
            {
                if !checksums.contains_key(isa)
                    && let Some(stock) = boot_image(&mut device, isa)?
                {
                    checksums.insert(isa.clone(), stock.split(':').map(str::to_owned).collect());
                }
            }
            for mut job in boot {
                let components = checksums.get_mut(&job.isa).expect("planned from it");
                job.before_checksums = components[..job.index].join(":");
                tracing::info!(image = %job.name(), isa = %job.isa, "compiling the boot image again");
                let (files, header) =
                    art.compile_boot(&job, &mut device)
                        .map_err(|source| Error::Dexopt {
                            dex: job.name(),
                            source: Box::new(source),
                        })?;
                components[job.index] = header.checksum();
                for (at, file) in files {
                    written.push((at, std::fs::read(&file).at(&file)?));
                }
            }
            for job in &jobs {
                tracing::info!(dex = %job.dex, isa = %job.isa, "compiling ART artifacts again");
                let expected = checksums[&job.isa].join(":");
                let files =
                    art.compile(job, &mut device, &expected)
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
        tracing::debug!(partition = %at.partition, path = %at.path, "writing a file compiled again");
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

/// The ROM's boot image for `isa`, as an oat file compiled against it records it
/// in `bootclasspath-checksums`: the primary image, `boot.art` in
/// `/system/framework/<isa>/`, then each extension there, an image that covers
/// jars and extends others, in the order they extend.
fn boot_image(device: &mut Device<'_>, isa: &str) -> Result<Option<String>, Error> {
    let dir = format!("/system/framework/{isa}");
    let Some(primary) = device.read(&format!("{dir}/boot.art"))? else {
        return Ok(None);
    };
    let mut images = vec![ImageHeader::read(&primary)?];
    for file in device.list(&dir)? {
        if file.starts_with("boot-") && file.ends_with(".art") {
            let header =
                ImageHeader::read(&device.read(&format!("{dir}/{file}"))?.unwrap_or_default())?;
            if header.component_count > 0 && header.boot_image_component_count > 0 {
                images.push(header);
            }
        }
    }
    images.sort_by_key(|image| image.boot_image_component_count);
    let checksums: Vec<String> = images.iter().map(ImageHeader::checksum).collect();
    Ok(Some(checksums.join(":")))
}

/// The images of the boot image an edit made stale, in order, and every odex
/// compiled against it that an edit made stale, with what compiling them again
/// needs.
fn stale(device: &mut Device<'_>) -> Result<(Vec<BootJob>, Vec<Job>), Error> {
    let mut missing = 0;
    let mut changed: BTreeMap<String, bool> = BTreeMap::new();
    let mut is_changed = |device: &mut Device<'_>, path: &str| -> Result<bool, Error> {
        if let Some(changed) = changed.get(path) {
            return Ok(*changed);
        }
        let result = device.changed(path)?;
        changed.insert(path.to_owned(), result);
        Ok(result)
    };
    let odexes: Vec<_> = odexes(device)?
        .into_iter()
        .map(|(artifacts, isa, dex)| Ok((header(device, &artifacts)?, artifacts, isa, dex)))
        .collect::<Result<_, Error>>()?;

    // The boot image of each instruction set, as an odex compiled against it
    // lays it out. An odex compiled against another, as some a device's own
    // partitions ship, is rejected already and left as it is.
    let mut checksums = BTreeMap::new();
    for (_, _, isa, _) in &odexes {
        if !checksums.contains_key(isa) {
            checksums.insert(isa.clone(), boot_image(device, isa)?);
        }
    }
    let ours = |oat: &Oat, isa: &str| {
        checksums[isa].is_some()
            && oat.store.get("bootclasspath-checksums") == checksums[isa].as_ref()
    };
    let mut boot = Vec::new();
    let mut boot_stale = BTreeMap::new();
    for (oat, _, isa, _) in &odexes {
        if boot_stale.contains_key(isa) || !ours(oat, isa) {
            continue;
        }
        let jars: Vec<String> = oat
            .get("bootclasspath")?
            .split(':')
            .map(str::to_owned)
            .collect();
        let mut start = 0;
        let mut stale = false;
        for (index, count) in
            android::art::boot_image_components(oat.get("bootclasspath-checksums")?)?
                .into_iter()
                .enumerate()
        {
            let own = &jars[start..start + count];
            let mut originals = Vec::new();
            for jar in own {
                if is_changed(device, jar)? {
                    stale = true;
                    if let Some(at) = device.locate(jar)?
                        && let Some(original) = device.original(&at)?
                    {
                        originals.push((jar.clone(), original));
                    }
                }
            }
            if stale {
                boot.push(BootJob {
                    isa: isa.clone(),
                    index,
                    before: jars[..start].to_vec(),
                    jars: own.to_vec(),
                    before_checksums: String::new(),
                    originals,
                });
            }
            start += count;
        }
        boot_stale.insert(isa.clone(), stale);
    }

    let mut jobs = Vec::new();
    for (odex, artifacts, isa, dex) in odexes {
        if !ours(&odex, &isa) {
            continue;
        }
        let own = is_changed(device, &dex)?;
        let mut stale = own || boot_stale[&isa];
        let context = dexopt::paths(odex.store.get("classpath").map_or("", |s| s));
        for jar in &context {
            stale |= is_changed(device, jar)?;
        }
        if !stale {
            continue;
        }
        // An odex whose class loader context names a jar the build lacks cannot
        // be compiled as the ROM's was, and ART rejects it already: the package
        // manager gives the app no such jar. ART compiles the app on the device.
        let mut absent = None;
        for jar in &context {
            if !device.exists(jar)? {
                absent = Some(jar.clone());
                break;
            }
        }
        if let Some(jar) = absent {
            tracing::debug!(dex = %dex, jar = %jar, "odex left stale: its class loader context names a jar the build lacks");
            missing += 1;
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
    if missing > 0 {
        tracing::warn!(
            missing,
            "odex files left stale, as their class loader context names a jar the build lacks"
        );
    }
    Ok((boot, jobs))
}

/// The oat header of the odex at `artifacts` (without `.odex`), from the start
/// of the file.
fn header(device: &Device<'_>, artifacts: &PartitionPath) -> Result<Oat, Error> {
    let target = &device.targets[&artifacts.partition];
    let file = target.file(&relative(format!("{}.odex", artifacts.path)))?;
    let mut start = Vec::new();
    target
        .store(device.images)
        .open(file.content, file.size)?
        .take(HEADER)
        .read_to_end(&mut start)
        .at(file.path.as_ref())?;
    Ok(Oat::read(&start)?)
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
