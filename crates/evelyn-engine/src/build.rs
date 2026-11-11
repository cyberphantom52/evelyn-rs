//! The build pipeline: open the firmware, pick the port, resolve the partitions,
//! extract and scan what the build reads, apply the recipes, build the images.
//!
//! Source images are never unpacked. Each one is scanned into a tree whose files
//! refer back to the image, and stays open for the whole build: untouched files
//! stream straight from it into the new image, and only edited files are written.
//!
//! Work directory layout:
//!   firmware/{rom,stock}/               partitions extracted from an OTA package
//!   targets/<partition>/                the blobs edits wrote
//!   packages/                           decoded APK/JAR workspaces
//!   images/<partition>.img              built filesystem images

use crate::firmware::{Firmware, Given};
use crate::properties::{check_fingerprint, port_for, rom_properties};
use crate::{Error, Output, Profile, profile};
use android::firmware::Firmware as Source;
use android::firmware::{Filesystem, Mounts, Name, STANDARD_SYSTEM, SourceRef, VENDOR_SIDE};
use android::lp::SuperImage;
use android::{Image, PartitionTree};
use recipe::Recipe;
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
use toolchain::Tools;

/// A build of a device repository.
pub struct Build<'a> {
    /// The repository's root, where `evelyn.toml` is.
    pub root: &'a Path,
    /// The ROM and the stock firmware: each a directory of partition images or
    /// an OTA package, local or a URL [default: `firmware/rom` and
    /// `firmware/stock` in the repository].
    pub rom: Option<Given>,
    pub stock: Option<Given>,
    /// [default: a super image for a device with a `super` layout, else
    /// partition images]
    pub output: Option<Output>,
    pub work_dir: &'a Path,
    /// [default: `out/<port>` in the repository]
    pub output_dir: Option<&'a Path>,
    /// Patch directories applied after the port's.
    pub extra_patches: &'a [PathBuf],
    /// Compose and apply every recipe, but build and write no images.
    pub dry_run: bool,
    pub tools: &'a Tools,
}

/// What a build did.
#[derive(Debug, Serialize)]
pub struct Built {
    /// The port the ROM matched.
    pub port: Name,
    /// In `super` order.
    pub partitions: Vec<Composed>,
    /// The recipes applied, in order.
    pub recipes: Vec<Name>,
    /// ROM partitions the port does not take.
    pub ignored: Vec<Name>,
    /// Steps whose bindings or condition did not apply here.
    pub skipped: Vec<patch::Skipped>,
    /// What was written; nothing for a dry run.
    pub outputs: Vec<PathBuf>,
}

/// A partition the build composed.
#[derive(Debug, Serialize)]
pub struct Composed {
    pub name: Name,
    /// The firmware it is built from.
    pub from: Source,
    pub filesystem: Filesystem,
    pub mount_point: String,
}

/// A partition this build composes and builds into an image.
struct Partition {
    name: Name,
    from: Source,
    /// The port's override; otherwise the base image's filesystem.
    filesystem: Option<Filesystem>,
    mount_point: String,
}

/// A recipe, and the directory its assets are in, `None` for a recipe built into
/// Evelyn, whose assets are in the tools.
struct Loaded {
    recipe: Recipe,
    assets: Option<PathBuf>,
}

/// Recipes every build applies before the port's patches, by name and text.
const BUILT_IN: [(&str, &str); 1] =
    [("vndk.ron", include_str!("../../../assets/recipes/vndk.ron"))];

/// Build the repository: the recipes built into Evelyn, the port's patches, then
/// the extra patch directories, applied to the partitions the port takes. Of
/// firmware given as an OTA package, only the partitions the build reads are
/// extracted, into the work directory, and removed when the build returns: the
/// ROM's system side first, whose properties pick the port, then the device's
/// partitions, then what the port takes and its patches read.
pub fn build(build: &Build<'_>) -> Result<Built, Error> {
    let given = |given: &Option<Given>, default: &str| {
        given
            .clone()
            .unwrap_or_else(|| Given::Path(build.root.join("firmware").join(default)))
    };
    let scratch = build.work_dir.join("firmware");
    let mut rom = Firmware::open(&given(&build.rom, "rom"), &scratch.join("rom"))?;
    let mut stock = Firmware::open(&given(&build.stock, "stock"), &scratch.join("stock"))?;

    let system: Vec<Name> = rom
        .names()
        .filter(|name| STANDARD_SYSTEM.contains(&name.as_ref()))
        .cloned()
        .collect();
    rom.extract(&system)?;
    let profile = Profile::load(build.root, &port_for(build.root, &rom_properties(&rom)?)?)?;
    let output = build.output.unwrap_or(match profile.device.super_layout {
        Some(_) => Output::Super,
        None => Output::Partitions,
    });
    if output != Output::Partitions && profile.device.super_layout.is_none() {
        return Err(Error::MissingLayout);
    }

    let (mut partitions, ignored) = rom_partitions(&profile, &rom)?;
    partitions.extend(device_partitions(&profile, &mut stock)?);
    let mounts = Mounts::new(
        partitions
            .iter()
            .map(|partition| (partition.mount_point.clone(), partition.name.clone())),
    );

    let recipes = recipes(&profile, build.extra_patches)?;

    let read = read(&partitions, &recipes, &mounts)?;
    let of = |source: Source| -> Vec<Name> {
        read.iter()
            .filter(|read| read.source == source)
            .map(|read| read.partition.clone())
            .collect()
    };
    rom.extract(&of(Source::Rom))?;
    stock.extract(&of(Source::Stock))?;

    let mut images = patch::Images::new();
    let mut sources = patch::Sources::new();
    for source in read {
        let firmware = match source.source {
            Source::Rom => &rom,
            Source::Stock => &stock,
        };
        // Steps that run check their imports; a skipped step needs none.
        let Some(path) = firmware.image(&source.partition) else {
            continue;
        };
        let image = Image::detect(path).map_err(|_| Error::SourceFilesystem(source.clone()))?;
        tracing::info!(%source, "scanning source image");
        sources.insert(source, android::scan(&image, images.len())?);
        images.push(image);
    }

    let (targets, skipped) = compose(build, &partitions, &recipes, &sources, &images, &mounts)?;
    for skip in &skipped {
        tracing::warn!(recipe = %skip.recipe, step = %skip.step, reason = %skip.reason, "skipped step");
    }
    crate::fstab::check(&targets, &images)?;
    let mut built = Built {
        port: profile.name.clone(),
        partitions: partitions
            .iter()
            .map(|partition| Composed {
                name: partition.name.clone(),
                from: partition.from,
                filesystem: targets[&partition.name].1.filesystem,
                mount_point: partition.mount_point.clone(),
            })
            .collect(),
        recipes: recipes
            .iter()
            .map(|recipe| recipe.recipe.name.clone())
            .collect(),
        ignored,
        skipped,
        outputs: Vec::new(),
    };
    if build.dry_run {
        return Ok(built);
    }

    let written = images_of(
        build.work_dir,
        &partitions,
        &targets,
        &images,
        &profile,
        output,
    )?;
    let output_dir = build
        .output_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| build.root.join("out").join(profile.name.as_ref()));
    built.outputs = crate::output::publish(&output_dir, &profile, output, &stock, written)?;
    Ok(built)
}

/// The ROM's partitions the port takes, the standard system side first, and
/// those it leaves out.
fn rom_partitions(profile: &Profile, rom: &Firmware) -> Result<(Vec<Partition>, Vec<Name>), Error> {
    let mut partitions = Vec::new();
    let mut ignored = Vec::new();
    for name in STANDARD_SYSTEM {
        if let Some(name) = rom.names().find(|key| key.as_ref() == name) {
            partitions.push(partition(profile, name, Source::Rom, rom)?);
        }
    }
    for name in rom.names() {
        if STANDARD_SYSTEM.contains(&name.as_ref()) || profile.is_vendor_side(name.as_ref()) {
            continue;
        }
        if profile.takes_from_rom(name.as_ref()) {
            partitions.push(partition(profile, name, Source::Rom, rom)?);
        } else {
            tracing::warn!(partition = %name, "the ROM's partition is not in port.toml; leaving it out");
            ignored.push(name.clone());
        }
    }
    Ok((partitions, ignored))
}

/// The device's partitions: the vendor side the stock firmware has, and any other
/// partition `evelyn.toml` sets, each extracted and checked to be the pinned build.
fn device_partitions(profile: &Profile, stock: &mut Firmware) -> Result<Vec<Partition>, Error> {
    if let Some(missing) = profile
        .device
        .partitions
        .keys()
        .find(|name| !stock.has(name.as_ref()))
    {
        return Err(Error::MissingStockPartition(missing.clone()));
    }
    let mut device: Vec<Name> = stock
        .names()
        .filter(|name| {
            VENDOR_SIDE.contains(&name.as_ref()) || profile.device.partitions.contains_key(*name)
        })
        .cloned()
        .collect();
    device.sort_by_key(Profile::device_order);
    stock.extract(&device)?;
    let mut partitions = Vec::new();
    for name in &device {
        let image = stock.image(name).expect("extracted");
        check_fingerprint(name, image, &profile.device.stock.fingerprint)?;
        partitions.push(partition(profile, name, Source::Stock, stock)?);
    }
    Ok(partitions)
}

/// `name` from `firmware`, with the port's settings for it. Where `system` is
/// mounted, unless the port says, is decided from its image, which is extracted.
fn partition(
    profile: &Profile,
    name: &Name,
    from: Source,
    firmware: &Firmware,
) -> Result<Partition, Error> {
    let settings = profile
        .settings(name.as_ref())?
        .cloned()
        .unwrap_or_default();
    let mount_point = match settings
        .mount_point
        .or_else(|| android::firmware::default_mount_point(name.as_ref()))
    {
        Some(mount_point) => mount_point,
        None => {
            let image = firmware.image(name).expect("the system side is extracted");
            let found = Image::detect(image)?
                .read_files(|path| path == "system/build.prop" || path == "build.prop")?;
            android::firmware::system_mount_point(|path| found.iter().any(|(file, _)| file == path))
                .ok_or(Error::SystemLayout)?
                .to_owned()
        }
    };
    Ok(Partition {
        name: name.clone(),
        from,
        filesystem: settings.filesystem,
        mount_point,
    })
}

/// The recipes built into Evelyn, the port's patches, then the `extra` patch
/// directories'.
fn recipes(profile: &Profile, extra: &[PathBuf]) -> Result<Vec<Loaded>, Error> {
    let mut recipes = Vec::new();
    for (name, text) in BUILT_IN {
        let recipe = Recipe::parse(text, Path::new(name))?;
        recipes.push(Loaded {
            recipe,
            assets: None,
        });
    }
    let extra = extra.iter().map(|patch| patch.join(profile::PATCH));
    for path in profile.patches().into_iter().chain(extra) {
        let assets = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let recipe = Recipe::load(&path)?;
        recipes.push(Loaded {
            recipe,
            assets: Some(assets),
        });
    }
    Ok(recipes)
}

/// What the build reads of the firmware: each partition's base, and the firmware
/// partition of every path a recipe looks for in one.
fn read(
    partitions: &[Partition],
    recipes: &[Loaded],
    mounts: &Mounts,
) -> Result<BTreeSet<SourceRef>, Error> {
    let mut read: BTreeSet<SourceRef> = partitions
        .iter()
        .map(|partition| SourceRef {
            source: partition.from,
            partition: partition.name.clone(),
        })
        .collect();
    let mut reads = Vec::new();
    for recipe in recipes {
        reads.extend(crate::needs::reads(&recipe.recipe)?);
    }
    for (firmware, path) in reads {
        for found in mounts.resolve(&path) {
            read.insert(SourceRef {
                source: firmware,
                partition: found.partition,
            });
        }
    }
    Ok(read)
}

fn reset(path: &Path) -> Result<(), Error> {
    if path.exists() {
        fs::remove_dir_all(path)?;
    }
    fs::create_dir_all(path)?;
    Ok(())
}

pub(crate) type Targets = BTreeMap<Name, (PathBuf, PartitionTree)>;

/// Start each partition from its base tree and apply every recipe in order.
fn compose(
    build: &Build<'_>,
    partitions: &[Partition],
    recipes: &[Loaded],
    sources: &patch::Sources,
    images: &patch::Images,
    mounts: &Mounts,
) -> Result<(Targets, Vec<patch::Skipped>), Error> {
    let root = build.work_dir.join("targets");
    reset(&root)?;
    let mut targets = BTreeMap::new();
    for partition in partitions {
        let base = sources[&SourceRef {
            source: partition.from,
            partition: partition.name.clone(),
        }]
            .clone();
        let filesystem = partition.filesystem.unwrap_or(base.filesystem);
        let target = patch::Target::create(&root.join(partition.name.as_ref()), base, filesystem)?;
        targets.insert(partition.name.clone(), target);
    }
    let packages = build.work_dir.join("packages");
    reset(&packages)?;
    let origins = partitions
        .iter()
        .map(|partition| (partition.name.clone(), partition.from))
        .collect();
    let mut composition = patch::Composition::new(targets, origins);
    for recipe in recipes {
        tracing::info!(recipe = %recipe.recipe.name, "applying recipe");
        let context = patch::Context {
            sources,
            images,
            tools: build.tools,
            assets: recipe.assets.as_deref(),
            packages: &packages,
            mounts,
        };
        composition.apply(&recipe.recipe, &context)?;
    }
    let (trees, skipped) = composition.finish(mounts, images)?;
    let targets = trees
        .into_iter()
        .map(|(name, tree)| {
            let path = root.join(name.as_ref());
            (name, (path, tree))
        })
        .collect();
    Ok((targets, skipped))
}

/// Where a partition is built.
enum To<'a> {
    Region(android::Region),
    File(&'a Path),
}

/// What a build wrote: each partition's image in `super` order, or one `super` image
/// holding them all.
pub(crate) enum Written {
    Partitions(Vec<(Name, PathBuf)>),
    Super(PathBuf),
}

/// Build every partition into `images/` in the work directory: each into its own
/// image, or straight into its place in `super`.
fn images_of(
    work_dir: &Path,
    partitions: &[Partition],
    targets: &Targets,
    images: &patch::Images,
    profile: &Profile,
    output: Output,
) -> Result<Written, Error> {
    let dir = work_dir.join("images");
    reset(&dir)?;
    let build = |partition: &Partition, to: To| {
        let (root, tree) = &targets[&partition.name];
        tracing::info!(partition = %partition.name, filesystem = ?tree.filesystem, "building image");
        let store = android::Store::new(images, root);
        let (name, filesystem, mount) = (&partition.name, tree.filesystem, &partition.mount_point);
        Ok::<_, Error>(match to {
            To::Region(region) => {
                android::build::image(tree, &store, region, filesystem, name, mount)?
            }
            To::File(image) => {
                android::build::image_file(tree, &store, image, filesystem, name, mount)?
            }
        })
    };
    Ok(match output {
        Output::Partitions => {
            let mut written = Vec::new();
            for partition in partitions {
                let image = dir.join(format!("{}.img", partition.name));
                build(partition, To::File(&image))?;
                written.push((partition.name.clone(), image));
            }
            Written::Partitions(written)
        }
        Output::Super | Output::Zip => {
            let layout = profile
                .device
                .super_layout
                .clone()
                .expect("checked when the output was chosen");
            let path = dir.join("super.img");
            let mut image = SuperImage::create(&path, layout)?;
            for partition in partitions {
                let size = build(partition, To::Region(image.region()?))?;
                image.add(partition.name.clone(), size)?;
            }
            image.finish()?;
            Written::Super(path)
        }
    })
}
