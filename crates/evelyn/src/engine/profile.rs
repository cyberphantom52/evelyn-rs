//! Device repositories and their ports.
//!
//! A device repository ports one ROM to one device. `evelyn.toml` at its root
//! names the ROM and describes the device: its product IDs, the stock build its
//! partitions are pinned to and its `super` layout. Each port, one ROM version,
//! is a directory under `ports/` whose `port.toml` says which ROM builds it is
//! for, what it takes from the ROM and, in the order they apply, the patches it
//! applies; a build takes the port the ROM it is given matches. A patch is a
//! directory under `patches/` holding `patch.ron` and its assets; nothing applies
//! unless a port lists it. Neither file names a firmware build: the firmware is
//! supplied when building.
//!
//! Partitions follow the Treble split. The device's own partitions (the vendor side
//! the stock firmware supplies) always come from it, pinned by build fingerprint;
//! the standard system side always comes from the ROM, and any other ROM partition
//! only when `port.toml` lists it (by name or glob).

use android::Filesystem;
use android::lp::SuperLayout;
use android::partition::{STANDARD_SYSTEM, VENDOR_SIDE};
use android::path::{RelativePath, glob};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};
use thiserror::Error;

/// The repository's manifest, at its root.
const MANIFEST: &str = "evelyn.toml";
/// A port's settings, in its directory.
const PORT: &str = "port.toml";
/// A patch's recipe, in its directory.
pub(crate) const PATCH: &str = "patch.ron";
/// What a repository keeps out of git: the work directory, the firmware and
/// what builds write.
const IGNORED: [&str; 3] = ["/.evelyn/", "/firmware/", "/out/"];

#[derive(Debug, Error)]
pub enum Error {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid profile {path}: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("invalid profile: {0}")]
    Invalid(String),
}

/// `evelyn.toml`: the ROM a repository ports and the device it ports it to.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Repository {
    /// The ROM the repository ports, for whoever reads the manifest.
    #[serde(rename = "rom")]
    _rom: String,
    device: Device,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Device {
    pub name: String,
    /// `ro.product.device` values the installer accepts.
    pub product_ids: Vec<String>,
    /// The stock firmware build the device's partitions must come from.
    pub stock: Stock,
    /// Settings for the device's own partitions, where the defaults are wrong.
    #[serde(default)]
    pub partitions: BTreeMap<String, Settings>,
    #[serde(rename = "super", default)]
    pub super_layout: Option<SuperLayout>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Stock {
    /// Each device partition's `ro.<partition>.build.fingerprint` must equal this.
    pub fingerprint: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Port {
    /// The ROM builds this port is for: properties the ROM sets, each to a value
    /// its glob matches. A port without any is for every build.
    #[serde(default)]
    pub rom: BTreeMap<String, String>,
    /// ROM partitions beyond the standard system side, and settings for any
    /// system-side partition. Keys are names or globs; the most specific wins.
    #[serde(default)]
    pub partitions: BTreeMap<String, Settings>,
    /// The patches this port applies, in order: directories under `patches/`.
    #[serde(default)]
    pub patches: Vec<RelativePath>,
}

impl Port {
    /// Whether a ROM that sets `properties` is one this port is for.
    pub(crate) fn matches(&self, properties: &BTreeMap<String, String>) -> bool {
        self.rom.iter().all(|(key, pattern)| {
            properties
                .get(key)
                .is_some_and(|value| glob(pattern, value))
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Settings {
    /// Default: the source image's filesystem.
    pub filesystem: Option<Filesystem>,
    /// Default: `/<name>`; for `system`, `/` or `/system` as its image is laid out.
    pub mount_point: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct Profile {
    pub device: Device,
    /// The port's name: its directory under `ports/`.
    pub name: String,
    pub port: Port,
    /// The repository root, where `evelyn.toml` is.
    pub root: PathBuf,
}

impl Profile {
    /// Load the port `name` of the repository at `root`.
    pub(crate) fn load(root: &Path, name: &str) -> Result<Self, Error> {
        let repository: Repository = toml_file(&root.join(MANIFEST))?;
        let port: Port = toml_file(&root.join("ports").join(name).join(PORT))?;
        let profile = Self {
            device: repository.device,
            name: name.to_owned(),
            port,
            root: root.to_path_buf(),
        };
        profile.validate()?;
        Ok(profile)
    }

    fn validate(&self) -> Result<(), Error> {
        if self.device.product_ids.is_empty() {
            return Err(Error::Invalid(format!(
                "{MANIFEST} sets no device.product_ids"
            )));
        }
        if self.device.stock.fingerprint.is_empty() {
            return Err(Error::Invalid(format!(
                "{MANIFEST} sets no device.stock.fingerprint"
            )));
        }
        if let Some(standard) = self
            .device
            .partitions
            .keys()
            .find(|name| STANDARD_SYSTEM.contains(&name.as_str()))
        {
            return Err(Error::Invalid(format!(
                "device partition {standard} is system side, which comes from the ROM"
            )));
        }
        let settings = self
            .device
            .partitions
            .iter()
            .map(|(name, settings)| (name.as_str(), settings));
        for (pattern, settings) in settings.chain(
            self.port
                .partitions
                .iter()
                .map(|(pattern, settings)| (pattern.as_str(), settings)),
        ) {
            if settings
                .mount_point
                .as_ref()
                .is_some_and(|mount| !mount.starts_with('/'))
            {
                return Err(Error::Invalid(format!(
                    "partition {pattern} mount point must be absolute"
                )));
            }
        }
        for pattern in self.port.partitions.keys() {
            if let Some(vendor) = self.vendor_side().find(|vendor| glob(pattern, vendor)) {
                return Err(Error::Invalid(format!(
                    "port partition {pattern} matches {vendor}, which comes from the stock firmware; set it in {MANIFEST}"
                )));
            }
        }
        let mut listed = BTreeSet::new();
        for patch in &self.port.patches {
            if !listed.insert(patch) {
                return Err(Error::Invalid(format!("patch {patch} is listed twice")));
            }
            if !self.patch(patch).is_file() {
                return Err(Error::Invalid(format!(
                    "patch {patch} has no patches/{patch}/{PATCH}"
                )));
            }
        }
        Ok(())
    }

    /// The recipes of the patches this port applies, in the order they apply.
    pub(crate) fn patches(&self) -> Vec<PathBuf> {
        self.port
            .patches
            .iter()
            .map(|patch| self.patch(patch))
            .collect()
    }

    fn patch(&self, patch: &RelativePath) -> PathBuf {
        self.root.join("patches").join(patch.as_ref()).join(PATCH)
    }

    /// Settings for `partition` from its most specific entry: an exact name beats
    /// any glob, and a glob with more literal characters beats one with fewer.
    pub(crate) fn settings(&self, partition: &str) -> Result<Option<&Settings>, Error> {
        if let Some((_, settings)) = self
            .device
            .partitions
            .iter()
            .find(|(name, _)| name.as_str() == partition)
        {
            return Ok(Some(settings));
        }
        if let Some(settings) = self.port.partitions.get(partition) {
            return Ok(Some(settings));
        }
        let mut matches: Vec<_> = self
            .port
            .partitions
            .iter()
            .filter(|(pattern, _)| glob(pattern, partition))
            .map(|(pattern, settings)| (literals(pattern), pattern, settings))
            .collect();
        matches.sort_by_key(|(literals, ..)| std::cmp::Reverse(*literals));
        match matches.as_slice() {
            [] => Ok(None),
            [(first, a, settings), (second, b, other), ..]
                if first == second && settings != other =>
            {
                Err(Error::Invalid(format!(
                    "partition {partition} matches {a} and {b} equally, with different settings"
                )))
            }
            [(.., settings), ..] => Ok(Some(settings)),
        }
    }

    /// Treble's vendor-side names and the device's own partitions.
    fn vendor_side(&self) -> impl Iterator<Item = &str> {
        VENDOR_SIDE
            .into_iter()
            .chain(self.device.partitions.keys().map(|name| name.as_str()))
    }

    pub(crate) fn is_vendor_side(&self, partition: &str) -> bool {
        self.vendor_side().any(|vendor| vendor == partition)
    }

    /// `super` order for the device's partitions: Treble's order, then by name.
    pub(crate) fn device_order(partition: &str) -> (usize, String) {
        (
            VENDOR_SIDE
                .iter()
                .position(|vendor| *vendor == partition)
                .unwrap_or(VENDOR_SIDE.len()),
            partition.to_owned(),
        )
    }

    /// A ROM partition this port builds into `super`.
    pub(crate) fn takes_from_rom(&self, partition: &str) -> bool {
        STANDARD_SYSTEM.contains(&partition)
            || (!self.is_vendor_side(partition)
                && self
                    .port
                    .partitions
                    .keys()
                    .any(|pattern| glob(pattern, partition)))
    }
}

/// A new device repository.
#[derive(Debug, Serialize)]
pub struct Initialized {
    pub root: PathBuf,
    /// The manifest, for the device's stock fingerprint, product IDs and
    /// `super` layout to be filled in.
    pub manifest: PathBuf,
}

/// Set up a device repository at `root` that ports `rom` to `device`: its
/// manifest, the `ports/`, `patches/` and `firmware/` directories, and what git
/// ignores. One already there is left alone.
pub fn init(root: &Path, device: &str, rom: &str) -> Result<Initialized, Error> {
    let manifest = root.join(MANIFEST);
    if manifest.exists() {
        return Err(Error::Invalid(format!(
            "{} is already a device repository",
            root.display()
        )));
    }
    let write = |path: &Path| {
        let path = path.to_path_buf();
        move |source| Error::Write { path, source }
    };
    let dirs = ["ports", "patches", "firmware/stock", "firmware/rom"].map(|dir| root.join(dir));
    for dir in dirs {
        fs::create_dir_all(&dir).map_err(write(&dir))?;
    }
    let text = format!(
        r#"# {rom} ported to {device}.
rom = "{rom}"

[device]
name = "{device}"
# The ro.product.device values the installer accepts.
product_ids = []
# Settings for the device's own partitions, where the defaults are wrong:
# partitions.<name> = {{ filesystem = "ext4", mount_point = "/<name>" }}

# The stock build the device's partitions come from: each device partition's
# ro.<partition>.build.fingerprint must equal it.
[device.stock]
fingerprint = ""

# The super partition, for super and ZIP output.
# [device.super]
# size = 0
# metadata_size = 65536
# slots = "virtual_ab"
# group = ""

# A port is ports/<name>/port.toml: the patches it applies, in order, as
# directories under patches/, each holding a {PATCH}; the ROM builds it is for,
# as globs over properties the ROM sets; and any ROM partitions it takes beyond
# the standard system side. A build takes the port the ROM matches.
#
# patches = ["<topic>/<name>"]
#
# [rom]
# "ro.build.version.release" = "17"
#
# [partitions]
# "my_*" = {{}}
#
# The firmware a build reads: the stock firmware in firmware/stock and the ROM
# in firmware/rom, each a directory of <partition>.img images or an OTA package.
"#
    );
    fs::write(&manifest, text).map_err(write(&manifest))?;
    let ignore = root.join(".gitignore");
    let ignored = match fs::read_to_string(&ignore) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(Error::Read {
                path: ignore,
                source,
            });
        }
    };
    let missing: String = IGNORED
        .iter()
        .filter(|entry| !ignored.lines().any(|line| line == **entry))
        .map(|entry| format!("{entry}\n"))
        .collect();
    if !missing.is_empty() {
        let separator = if ignored.is_empty() || ignored.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        fs::write(&ignore, format!("{ignored}{separator}{missing}")).map_err(write(&ignore))?;
    }
    Ok(Initialized {
        root: root.to_path_buf(),
        manifest,
    })
}

/// The repository `start` is in: the nearest directory with `evelyn.toml`,
/// `start` or one above it.
pub fn find(start: &Path) -> Result<PathBuf, Error> {
    let start = fs::canonicalize(start).map_err(|source| Error::Read {
        path: start.into(),
        source,
    })?;
    start
        .ancestors()
        .find(|dir| dir.join(MANIFEST).is_file())
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            Error::Invalid(format!(
                "{} is not in a device repository: no {MANIFEST} here or above",
                start.display()
            ))
        })
}

/// The ports of the repository at `root`, by name.
pub(crate) fn ports(root: &Path) -> Result<BTreeMap<String, Port>, Error> {
    let dir = root.join("ports");
    let entries = fs::read_dir(&dir).map_err(|source| Error::Read {
        path: dir.clone(),
        source,
    })?;
    let mut ports = BTreeMap::new();
    for entry in entries {
        let path = entry
            .map_err(|source| Error::Read {
                path: dir.clone(),
                source,
            })?
            .path();
        if !path.join(PORT).is_file() {
            continue;
        }
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        ports.insert(name, toml_file(&path.join(PORT))?);
    }
    Ok(ports)
}

fn toml_file<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, Error> {
    let text = fs::read_to_string(path).map_err(|source| Error::Read {
        path: path.into(),
        source,
    })?;
    toml::from_str(&text).map_err(|source| Error::Toml {
        path: path.into(),
        source,
    })
}

fn literals(pattern: &str) -> usize {
    pattern.chars().filter(|c| !matches!(c, '*' | '?')).count()
}
