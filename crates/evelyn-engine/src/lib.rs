//! Firmware composition planning and build execution.

mod authoring;
mod build;
mod firmware;
mod flashable;
mod fstab;
mod needs;
mod output;
pub mod profile;
pub use authoring::{
    CaptureKind, CaptureRequest, CapturedRecipe, PrepareRequest, PreparedFile, capture_file,
    prepare_file, preview_trees,
};
pub use build::{BuildRequest, Built, build};
pub use firmware::{Extracted, extract_in_place};
pub use profile::{Profile, init};

use android_firmware::Firmware;
use android_firmware::{Filesystem, Mounts, Name, SourceRef};
use android_firmware::{STANDARD_SYSTEM, VENDOR_SIDE};
use android_fs::{Artifact, ImageFormat};
use recipe::Recipe;
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("source {0} is not EROFS or ext4")]
    SourceFilesystem(SourceRef),
    #[error(transparent)]
    Patch(#[from] patch::Error),
    #[error(transparent)]
    Capture(#[from] patch::capture::Error),
    #[error(transparent)]
    Preview(#[from] patch::preview::Error),
    #[error("invalid JSON record: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
    #[error(transparent)]
    Android(#[from] android_fs::Error),
    #[error(transparent)]
    Toolchain(#[from] toolchain::Error),
    #[error("workspace IO: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Recipe(#[from] recipe::Error),
    #[error(transparent)]
    Profile(#[from] profile::Error),
    #[error(transparent)]
    Firmware(#[from] android_firmware::Error),
    #[error("step IDs must be unique across recipes: {0}")]
    DuplicateStep(Name),
    #[error("{fstab} mounts {partition}, which this build does not have")]
    FstabPartition { fstab: String, partition: String },
    #[error("{fstab} mounts {partition} as {accepted:?}, but it is built as {filesystem}")]
    FstabFilesystem {
        fstab: String,
        partition: String,
        filesystem: Filesystem,
        accepted: Vec<String>,
    },
    #[error("ZIP output flashes the stock firmware's images from {0}, which does not exist")]
    MissingFirmware(PathBuf),
    #[error("the stock firmware has no {0}.img, which evelyn.toml sets")]
    MissingStockPartition(Name),
    #[error(
        "stock {partition} is not the build evelyn.toml pins: expected {expected}, found {found}"
    )]
    StockFingerprint {
        partition: Name,
        expected: String,
        found: String,
    },
    #[error(
        "the system image has neither system/build.prop nor build.prop; set its mount_point in port.toml"
    )]
    SystemLayout,
    #[error("super and ZIP output need a [device.super] section in evelyn.toml")]
    MissingLayout,
    #[error("missing regular recipe asset: {0}")]
    MissingAsset(PathBuf),
    #[error("invalid authoring request: {0}")]
    Authoring(&'static str),
    #[error("OTA package: {0}")]
    Payload(#[from] payload_dumper::Error),
    #[error("{0} is an incremental OTA; give a full one")]
    IncrementalOta(PathBuf),
    #[error("{image}, extracted from {package}, does not match the hash its manifest records")]
    OtaHash { package: PathBuf, image: PathBuf },
    #[error("{0} holds more than one OTA package")]
    AmbiguousFirmware(PathBuf),
    #[error("{0} holds both an OTA package and images: keep one")]
    MixedFirmware(PathBuf),
    #[error("the ROM matches no port ({0})")]
    NoPort(String),
    #[error("the ROM matches more than one port: {0}")]
    Ports(String),
    #[error("the ROM is not one port {0} is for")]
    PortMismatch(Name),
}

/// What a build produces: partition images, a super image, or a flashable ZIP.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Output {
    Partitions,
    Super,
    Zip,
}

#[derive(Debug, Serialize)]
pub struct PlannedSource {
    pub source: SourceRef,
    pub artifact: Artifact,
}

#[derive(Debug, Serialize)]
pub struct PlannedRecipe {
    /// The patch's `patch.ron`, its assets beside it; `None` for a recipe built
    /// into Evelyn, whose assets are in the tools.
    pub path: Option<PathBuf>,
    pub recipe: Recipe,
    /// The recipe as primitives, which is what is checked and applied.
    #[serde(skip)]
    pub tree: recipe::tree::Tree,
}

/// A partition this build composes and builds into an image.
#[derive(Debug, Serialize)]
pub struct PlannedPartition {
    pub name: Name,
    pub base: SourceRef,
    /// The port's override; otherwise the base image's filesystem.
    pub filesystem: Option<Filesystem>,
    /// Where the partition is mounted; for `system` without an override, decided
    /// from its image when building.
    pub mount_point: Option<String>,
}

/// What a build is of, where it is not the repository's default: each firmware a
/// directory of partition images or an OTA package, by default `firmware/rom` and
/// `firmware/stock` in the repository, and the port, by default the one the ROM
/// matches.
#[derive(Default)]
pub struct Inputs<'a> {
    pub rom: Option<&'a Path>,
    pub stock: Option<&'a Path>,
    pub port: Option<&'a Name>,
}

/// Recipes every build applies before the port's patches, by name and text.
const BUILT_IN: [(&str, &str); 1] =
    [("vndk.ron", include_str!("../../../assets/recipes/vndk.ron"))];

/// A port with its recipes loaded and the partitions the firmware supplies resolved.
#[derive(Debug, Serialize)]
pub struct Plan {
    pub profile: Profile,
    pub output: Output,
    /// In `super` order: standard system side, other ROM partitions, vendor side.
    pub partitions: Vec<PlannedPartition>,
    pub sources: Vec<PlannedSource>,
    pub recipes: Vec<PlannedRecipe>,
    /// Firmware images a flashable ZIP flashes, in name order.
    pub firmware: Vec<PathBuf>,
    /// ROM partitions the port does not take.
    pub ignored: Vec<Name>,
    /// OTA packages' images, extracted for this plan and removed with it.
    #[serde(skip)]
    _scratch: Vec<firmware::Scratch>,
}

impl Plan {
    /// Plan a build of the repository at `root` from `inputs`: the recipes built
    /// into Evelyn, the port's patches, then `extra` patch directories. Without
    /// an output, a device with a `super` layout gets a super image, and one
    /// without, partition images. Firmware given as an OTA package is extracted
    /// into `.evelyn/firmware`, kept only as long as the plan.
    pub fn load(
        root: &Path,
        inputs: &Inputs<'_>,
        output: Option<Output>,
        extra: &[PathBuf],
    ) -> Result<Self, Error> {
        let given = |input: Option<&Path>, default: &str| {
            input.map_or_else(|| root.join("firmware").join(default), Path::to_path_buf)
        };
        let extracted = root.join(".evelyn").join("firmware");
        let (rom_dir, rom_scratch) =
            firmware::images(&given(inputs.rom, "rom"), &extracted.join("rom"), false)?;
        let (stock_dir, stock_scratch) = firmware::images(
            &given(inputs.stock, "stock"),
            &extracted.join("stock"),
            true,
        )?;
        let rom = filesystems(images(&rom_dir)?)?;
        let stock = filesystems(images(&stock_dir)?)?;

        let properties = rom_properties(&rom)?;
        let name = match inputs.port {
            Some(port) => port.clone(),
            None => port_for(root, &properties)?,
        };
        let profile = Profile::load(root, &name)?;
        if !profile.port.matches(&properties) {
            return Err(Error::PortMismatch(name));
        }
        let output = output.unwrap_or(match profile.device.super_layout {
            Some(_) => Output::Super,
            None => Output::Partitions,
        });
        if output != Output::Partitions && profile.device.super_layout.is_none() {
            return Err(Error::MissingLayout);
        }
        let mut recipes = Vec::new();
        for (name, text) in BUILT_IN {
            let recipe = Recipe::parse(text, Path::new(name))?;
            let tree = recipe.tree()?;
            recipes.push(PlannedRecipe {
                path: None,
                recipe,
                tree,
            });
        }
        let extra = extra.iter().map(|patch| patch.join(profile::PATCH));
        for recipe_path in profile.patches().into_iter().chain(extra) {
            let recipe = Recipe::load(&recipe_path)?;
            let tree = recipe.tree()?;
            let assets = recipe_path.parent().unwrap_or(Path::new("."));
            let mut needed = needs::Assets::default();
            tree.walk(&mut needed)?;
            for asset in &needed.0 {
                let asset = assets.join(asset.as_ref());
                if !asset.exists() {
                    return Err(Error::MissingAsset(asset));
                }
            }
            recipes.push(PlannedRecipe {
                path: Some(recipe_path),
                recipe,
                tree,
            });
        }
        let mut step_ids = BTreeSet::new();
        for planned in &recipes {
            profile.validate_recipe(&planned.tree)?;
            for step in &planned.recipe.steps {
                if !step_ids.insert(step.id.clone()) {
                    return Err(Error::DuplicateStep(step.id.clone()));
                }
            }
        }

        let mut partitions = Vec::new();
        let mut ignored = Vec::new();
        let mut plan_partition = |name: &Name, source: Firmware| -> Result<(), Error> {
            let settings = profile
                .settings(name.as_ref())?
                .cloned()
                .unwrap_or_default();
            partitions.push(PlannedPartition {
                name: name.clone(),
                base: SourceRef {
                    source,
                    partition: name.clone(),
                },
                filesystem: settings.filesystem,
                mount_point: settings
                    .mount_point
                    .or_else(|| android_firmware::default_mount_point(name.as_ref())),
            });
            Ok(())
        };
        for name in STANDARD_SYSTEM {
            if let Some(name) = rom.keys().find(|key| key.as_ref() == name) {
                plan_partition(name, Firmware::Rom)?;
            }
        }
        for name in rom.keys() {
            if STANDARD_SYSTEM.contains(&name.as_ref()) || profile.is_vendor_side(name.as_ref()) {
                continue;
            }
            if profile.takes_from_rom(name.as_ref()) {
                plan_partition(name, Firmware::Rom)?;
            } else {
                tracing::warn!(partition = %name, "the ROM's partition is not in port.toml; leaving it out");
                ignored.push(name.clone());
            }
        }
        // The device's partitions: the vendor side the stock firmware has, and any
        // other partition evelyn.toml sets.
        if let Some(missing) = profile
            .device
            .partitions
            .keys()
            .find(|name| !stock.contains_key(*name))
        {
            return Err(Error::MissingStockPartition(missing.clone()));
        }
        let mut device: Vec<&Name> = stock
            .keys()
            .filter(|name| {
                VENDOR_SIDE.contains(&name.as_ref())
                    || profile.device.partitions.contains_key(*name)
            })
            .collect();
        device.sort_by_key(|name| Profile::device_order(name));
        for name in device {
            check_fingerprint(name, &stock[name], &profile.device.stock.fingerprint)?;
            plan_partition(name, Firmware::Stock)?;
        }

        let mut sources = Vec::new();
        for source in required_sources(&partitions, &recipes)? {
            let images = match source.source {
                Firmware::Rom => &rom,
                Firmware::Stock => &stock,
            };
            let Some(image) = images.get(&source.partition) else {
                // Steps that run check their imports; a skipped step needs none.
                continue;
            };
            sources.push(PlannedSource {
                artifact: Artifact::inspect(image)?,
                source,
            });
        }
        // A flashable ZIP also flashes every image in the stock firmware's firmware/.
        let firmware_images = if output == Output::Zip {
            let directory = stock_dir.join("firmware");
            if !directory.is_dir() {
                return Err(Error::MissingFirmware(directory));
            }
            images(&directory)?.into_values().collect()
        } else {
            Vec::new()
        };
        Ok(Self {
            profile,
            output,
            partitions,
            sources,
            recipes,
            firmware: firmware_images,
            ignored,
            _scratch: rom_scratch.into_iter().chain(stock_scratch).collect(),
        })
    }
}

/// A stock partition must be the build `evelyn.toml` pins: the property files init
/// loads from it set `ro.<partition>.build.fingerprint` to `expected`.
fn check_fingerprint(partition: &Name, image: &Path, expected: &str) -> Result<(), Error> {
    let key = format!("ro.{partition}.build.fingerprint");
    match loaded(partition, image)?.remove(&key) {
        Some(value) if value == expected => Ok(()),
        Some(value) => Err(Error::StockFingerprint {
            partition: partition.clone(),
            expected: expected.into(),
            found: value,
        }),
        None => {
            tracing::warn!(%partition, "stock partition sets no {key}; its build cannot be checked");
            Ok(())
        }
    }
}

/// The properties the ROM's standard system side sets, in the order init loads
/// them, a later assignment of a key winning.
fn rom_properties(rom: &BTreeMap<Name, PathBuf>) -> Result<BTreeMap<String, String>, Error> {
    let mut properties = BTreeMap::new();
    for name in STANDARD_SYSTEM {
        if let Some((partition, image)) = rom.iter().find(|(key, _)| key.as_ref() == name) {
            properties.extend(loaded(partition, image)?);
        }
    }
    Ok(properties)
}

/// The one port of the repository at `root` a ROM setting `properties` matches.
fn port_for(root: &Path, properties: &BTreeMap<String, String>) -> Result<Name, Error> {
    let ports = profile::ports(root)?;
    let matching: Vec<&Name> = ports
        .iter()
        .filter(|(_, port)| port.matches(properties))
        .map(|(name, _)| name)
        .collect();
    match matching.as_slice() {
        [name] => Ok((*name).clone()),
        [] => {
            // What the ROM sets of what the ports match on.
            let keys: BTreeSet<&String> = ports.values().flat_map(|port| port.rom.keys()).collect();
            let found = keys
                .into_iter()
                .map(|key| match properties.get(key) {
                    Some(value) => format!("{key}={value}"),
                    None => format!("{key} unset"),
                })
                .collect::<Vec<_>>()
                .join(", ");
            Err(Error::NoPort(found))
        }
        names => Err(Error::Ports(
            names
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>()
                .join(", "),
        )),
    }
}

/// What the property files init loads from `partition` assign, the last
/// assignment of a key winning.
fn loaded(partition: &Name, image: &Path) -> Result<BTreeMap<String, String>, Error> {
    // The groups init loads from this partition, as paths in its image; system is
    // mounted at /, so its files are under system/ in it.
    let mount = match partition.as_ref() {
        "system" => "/".to_owned(),
        other => format!("/{other}/"),
    };
    let groups: Vec<Vec<&str>> = android_firmware::PROPERTY_FILES
        .iter()
        .map(|group| {
            group
                .iter()
                .filter_map(|path| path.strip_prefix(mount.as_str()))
                .collect::<Vec<_>>()
        })
        .filter(|group| !group.is_empty())
        .collect();
    let read: BTreeMap<String, Vec<u8>> = android_fs::Image::detect(image)?
        .read_files(|path| groups.iter().flatten().any(|file| *file == path))?
        .into_iter()
        .collect();
    let mut assigned = BTreeMap::new();
    for group in &groups {
        let (canonical, legacy) = group.split_first().expect("a file");
        let loaded = match read.get(*canonical) {
            Some(bytes) => vec![bytes],
            None => legacy.iter().filter_map(|file| read.get(*file)).collect(),
        };
        for bytes in loaded {
            for entry in android_property::entries(&String::from_utf8_lossy(bytes)) {
                assigned.insert(entry.key.to_owned(), entry.value.to_owned());
            }
        }
    }
    Ok(assigned)
}

/// The `<partition>.img` files in a firmware directory.
fn images(directory: &Path) -> Result<BTreeMap<Name, PathBuf>, Error> {
    let mut images = BTreeMap::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        let Some(stem) = path
            .extension()
            .is_some_and(|extension| extension == "img")
            .then(|| path.file_stem())
            .flatten()
        else {
            continue;
        };
        let Ok(name) = Name::try_from(stem.to_string_lossy().into_owned()) else {
            continue;
        };
        if path.is_file() {
            images.insert(name, path);
        }
    }
    Ok(images)
}

/// The filesystem images (EROFS or ext4, raw or sparse) among `images`.
fn filesystems(images: BTreeMap<Name, PathBuf>) -> Result<BTreeMap<Name, PathBuf>, Error> {
    let mut filesystems = BTreeMap::new();
    for (name, path) in images {
        if ImageFormat::detect(&path)? != ImageFormat::Unknown {
            filesystems.insert(name, path);
        }
    }
    Ok(filesystems)
}

/// Partition bases plus the firmware partition of every import, deduplicated.
fn required_sources(
    partitions: &[PlannedPartition],
    recipes: &[PlannedRecipe],
) -> Result<Vec<SourceRef>, Error> {
    // Provisional mount points: `system`'s layout is only known from its image, but
    // either way `/system/...` is in `system`.
    let mounts = Mounts::new(partitions.iter().map(|partition| {
        (
            partition.mount_point.clone().unwrap_or_else(|| "/".into()),
            partition.name.clone(),
        )
    }));
    let mut sources: BTreeSet<SourceRef> = partitions
        .iter()
        .map(|partition| partition.base.clone())
        .collect();
    let mut reads = needs::Sources::default();
    for recipe in recipes {
        recipe.tree.walk(&mut reads)?;
    }
    for (firmware, path) in reads.reads {
        for found in mounts.resolve(&path) {
            sources.insert(SourceRef {
                source: firmware,
                partition: found.partition,
            });
        }
    }
    Ok(sources.into_iter().collect())
}
