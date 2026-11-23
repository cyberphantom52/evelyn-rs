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
//!   images/<partition>.img              built filesystem images

use crate::Firmware;
use crate::SourceRef;
use crate::engine::firmware::{Given, OpenFirmware};
use crate::engine::properties::{check_fingerprint, port_for, rom_properties};
use crate::engine::{Error, IoAt, Output, Profile, profile};
use crate::recipe::Recipe;
use crate::toolchain::Tools;
use android::Filesystem;
use android::lp::SuperImage;
use android::partition::{Mounts, STANDARD_SYSTEM, VENDOR_SIDE};
use android::{Image, PartitionTree};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

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
    /// Apply the debug recipe built into Evelyn after every other patch.
    pub debug: bool,
    /// Compile again the ART files an edit made stale.
    pub dexpreopt: bool,
    pub tools: &'a Tools,
}

/// What a build did.
#[derive(Debug, Serialize)]
pub struct Built {
    /// The port the ROM matched.
    pub port: String,
    /// In `super` order.
    pub partitions: Vec<Composed>,
    /// The recipes applied, in order, by path.
    pub recipes: Vec<PathBuf>,
    /// ROM partitions the port does not take.
    pub ignored: Vec<String>,
    /// Steps whose bindings or condition did not apply here.
    pub skipped: Vec<crate::patch::Skipped>,
    /// What was written; nothing for a dry run.
    pub outputs: Vec<PathBuf>,
}

/// A partition the build composed.
#[derive(Debug, Serialize)]
pub struct Composed {
    pub name: String,
    /// The firmware it is built from.
    pub from: Firmware,
    pub filesystem: Filesystem,
    pub mount_point: String,
}

/// A partition this build composes and builds into an image.
struct Partition {
    name: String,
    from: Firmware,
    /// The port's override; otherwise the base image's filesystem.
    filesystem: Option<Filesystem>,
    mount_point: String,
}

/// A recipe, the path it is reported by, and the directory its assets are in,
/// `None` for a recipe built into Evelyn, whose assets are in the tools.
struct Patch {
    recipe: Recipe,
    /// A port's patch by its path in the repository, a recipe built into Evelyn
    /// by its file name.
    path: PathBuf,
    assets: Option<PathBuf>,
}

/// Recipes every build applies after the port's patches, by file name and text.
const BUILT_IN_RECIPES: [(&str, &str); 1] = [(
    "vndk.ron",
    include_str!("../../../../assets/recipes/vndk.ron"),
)];

/// The recipe a debug build applies after every other patch, and its assets,
/// written to the work directory for the build.
const DEBUG_RECIPE: &str = include_str!("../../../../assets/recipes/debug.ron");
const DEBUG_ASSETS: [(&str, &str); 1] = [(
    "logcat.rc",
    include_str!("../../../../assets/recipes/debug/logcat.rc"),
)];

/// Build the repository: the port's patches, the extra patch directories, then
/// the recipes built into Evelyn, applied to the partitions the port takes. Of
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
    let mut rom = OpenFirmware::open(&given(&build.rom, "rom"), &scratch.join("rom"))?;
    let mut stock = OpenFirmware::open(&given(&build.stock, "stock"), &scratch.join("stock"))?;

    let system: Vec<String> = rom
        .names()
        .filter(|name| STANDARD_SYSTEM.contains(&name.as_str()))
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
    if output == Output::Zip {
        stock.check_firmware_images()?;
    }

    let (mut partitions, ignored) = rom_partitions(&profile, &rom)?;
    partitions.extend(device_partitions(&profile, &mut stock)?);
    let mounts = Mounts::new(
        partitions
            .iter()
            .map(|partition| (partition.mount_point.clone(), partition.name.clone())),
    );

    crate::recipe::kit::load(&profile.root)?;
    let mut recipes = recipes(&profile, build.extra_patches)?;
    if build.debug {
        recipes.push(debug_recipe(&build.work_dir.join("debug"))?);
    }

    let read = read(&partitions, &recipes, &mounts)?;
    let of = |source: Firmware| -> Vec<String> {
        read.iter()
            .filter(|read| read.source == source)
            .map(|read| read.partition.clone())
            .collect()
    };
    rom.extract(&of(Firmware::Rom))?;
    stock.extract(&of(Firmware::Stock))?;

    let mut images = crate::patch::Images::new();
    let mut sources = crate::patch::Sources::new();
    for source in read {
        let firmware = match source.source {
            Firmware::Rom => &rom,
            Firmware::Stock => &stock,
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
        tracing::warn!(recipe = %skip.recipe.display(), step = %skip.step, reason = %skip.reason, "skipped step");
    }
    crate::engine::fstab::check(&targets, &images)?;
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
        recipes: recipes.iter().map(|recipe| recipe.path.clone()).collect(),
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
        .unwrap_or_else(|| build.root.join("out").join(&profile.name));
    built.outputs = crate::engine::output::publish(&output_dir, &profile, output, &stock, written)?;
    Ok(built)
}

/// The ROM's partitions the port takes, the standard system side first, and
/// those it leaves out.
fn rom_partitions(
    profile: &Profile,
    rom: &OpenFirmware,
) -> Result<(Vec<Partition>, Vec<String>), Error> {
    let mut partitions = Vec::new();
    let mut ignored = Vec::new();
    for name in STANDARD_SYSTEM {
        if let Some(name) = rom.names().find(|key| key.as_str() == name) {
            partitions.push(partition(profile, name, Firmware::Rom, rom)?);
        }
    }
    for name in rom.names() {
        if STANDARD_SYSTEM.contains(&name.as_str()) || VENDOR_SIDE.contains(&name.as_str()) {
            continue;
        }
        if profile.takes_from_rom(name.as_str()) {
            partitions.push(partition(profile, name, Firmware::Rom, rom)?);
        } else {
            tracing::warn!(partition = %name, "the port does not take the ROM's partition; leaving it out");
            ignored.push(name.clone());
        }
    }
    Ok((partitions, ignored))
}

/// The device's partitions: the vendor side the stock firmware has, each
/// extracted and checked to be the pinned build.
fn device_partitions(profile: &Profile, stock: &mut OpenFirmware) -> Result<Vec<Partition>, Error> {
    let mut device: Vec<String> = stock
        .names()
        .filter(|name| VENDOR_SIDE.contains(&name.as_str()))
        .cloned()
        .collect();
    device.sort_by_key(|name| Profile::device_order(name));
    stock.extract(&device)?;
    let mut partitions = Vec::new();
    for name in &device {
        let image = stock.image(name).expect("extracted");
        check_fingerprint(name, image, &profile.device.fingerprint)?;
        partitions.push(partition(profile, name, Firmware::Stock, stock)?);
    }
    Ok(partitions)
}

/// `name` from `firmware`, with the settings `evelyn.toml` has for it. Where
/// `system` is mounted, unless `evelyn.toml` says, is decided from its image,
/// which is extracted.
fn partition(
    profile: &Profile,
    name: &str,
    from: Firmware,
    firmware: &OpenFirmware,
) -> Result<Partition, Error> {
    let settings = profile.settings(name)?;
    let mount_point = match settings
        .mount_point
        .or_else(|| android::partition::default_mount_point(name))
    {
        Some(mount_point) => mount_point,
        None => {
            let image = firmware.image(name).expect("the system side is extracted");
            let found = Image::detect(image)?
                .read_files(|path| path == "system/build.prop" || path == "build.prop")?;
            android::partition::system_mount_point(|path| {
                found.iter().any(|(file, _)| file == path)
            })
            .ok_or(Error::SystemLayout)?
            .to_owned()
        }
    };
    Ok(Partition {
        name: name.to_owned(),
        from,
        filesystem: settings.filesystem,
        mount_point,
    })
}

/// The port's patches, the `extra` patch directories', then the recipes built
/// into Evelyn.
fn recipes(profile: &Profile, extra: &[PathBuf]) -> Result<Vec<Patch>, Error> {
    let mut recipes = Vec::new();
    let patches = profile
        .patches()
        .into_iter()
        .map(|path| (profile.root.join(&path), path));
    let extra = extra.iter().map(|patch| {
        let path = patch.join(profile::PATCH);
        (path.clone(), path)
    });
    for (file, path) in patches.chain(extra) {
        let assets = file.parent().unwrap_or(Path::new(".")).to_path_buf();
        recipes.push(Patch {
            recipe: Recipe::load(&file)?,
            path,
            assets: Some(assets),
        });
    }
    for (name, text) in BUILT_IN_RECIPES {
        recipes.push(Patch {
            recipe: Recipe::parse(text, Path::new(name))?,
            path: PathBuf::from(name),
            assets: None,
        });
    }
    Ok(recipes)
}

/// The debug recipe, with its assets written to `assets`.
fn debug_recipe(assets: &Path) -> Result<Patch, Error> {
    fs::create_dir_all(assets).at(assets)?;
    for (name, text) in DEBUG_ASSETS {
        let path = assets.join(name);
        fs::write(&path, text).at(&path)?;
    }
    Ok(Patch {
        recipe: Recipe::parse(DEBUG_RECIPE, Path::new("debug.ron"))?,
        path: PathBuf::from("debug.ron"),
        assets: Some(assets.to_path_buf()),
    })
}

/// What the build reads of the firmware: each partition's base, and the firmware
/// partition of every path a recipe looks for in one.
fn read(
    partitions: &[Partition],
    recipes: &[Patch],
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
        reads.extend(crate::engine::needs::reads(&recipe.recipe)?);
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
        fs::remove_dir_all(path).at(path)?;
    }
    fs::create_dir_all(path).at(path)?;
    Ok(())
}

pub(crate) type Targets = BTreeMap<String, (PathBuf, PartitionTree)>;

/// Start each partition from its base tree and apply every recipe in order.
fn compose(
    build: &Build<'_>,
    partitions: &[Partition],
    recipes: &[Patch],
    sources: &crate::patch::Sources,
    images: &crate::patch::Images,
    mounts: &Mounts,
) -> Result<(Targets, Vec<crate::patch::Skipped>), Error> {
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
        let target = crate::patch::Target::create(&root.join(&partition.name), base, filesystem)?;
        targets.insert(partition.name.clone(), target);
    }
    let origins = partitions
        .iter()
        .map(|partition| (partition.name.clone(), partition.from))
        .collect();
    let mut composition = crate::patch::Composition::new(targets, origins);
    for recipe in recipes {
        tracing::info!(recipe = %recipe.path.display(), "applying recipe");
        let context = crate::patch::Context {
            sources,
            images,
            tools: build.tools,
            assets: recipe.assets.as_deref(),
            mounts,
        };
        composition
            .apply(&recipe.recipe, &recipe.path, &context)
            .map_err(|source| Error::Apply {
                recipe: recipe.path.clone(),
                source,
            })?;
    }
    // Each patched policy file is compiled once, as every step left it.
    composition.check_policies(&crate::patch::Context {
        sources,
        images,
        tools: build.tools,
        assets: None,
        mounts,
    })?;
    // Each document is written back before what derives from it is made.
    composition.write_back()?;
    crate::engine::derived::refresh(
        &mut composition.targets,
        &composition.origins,
        sources,
        images,
        mounts,
        build.tools,
        &build.work_dir.join("derived"),
        match (build.dry_run, build.dexpreopt) {
            (true, _) => crate::engine::derived::Dexpreopt::Count,
            (false, true) => crate::engine::derived::Dexpreopt::Compile,
            (false, false) => crate::engine::derived::Dexpreopt::Skip,
        },
    )?;
    let (trees, skipped) = composition.finish(mounts, images)?;
    let targets = trees
        .into_iter()
        .map(|(name, tree)| {
            let path = root.join(&name);
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
    Partitions(Vec<(String, PathBuf)>),
    Super(PathBuf),
}

/// Build every partition into `images/` in the work directory: each into its own
/// image, or straight into its place in `super`.
fn images_of(
    work_dir: &Path,
    partitions: &[Partition],
    targets: &Targets,
    images: &crate::patch::Images,
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
