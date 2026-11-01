//! Device repositories and their ports.
//!
//! A device repository ports one ROM to one device. `evelyn.toml` at its root
//! names the ROM and describes the device: its product IDs, the stock build its
//! partitions are pinned to and its `super` layout. Each port, one ROM version,
//! is a directory under `ports/` whose `port.toml` says what it takes from the
//! ROM and lists, in the order they apply, the patches it applies. A patch is a
//! directory under `patches/` holding `patch.ron` and its assets; nothing applies
//! unless a port lists it. Neither file names a firmware build: the firmware is
//! supplied when building.
//!
//! Partitions follow the Treble split. The device's own partitions (the vendor side
//! the stock firmware supplies) always come from it, pinned by build fingerprint;
//! the standard system side always comes from the ROM, and any other ROM partition
//! only when `port.toml` lists it (by name or glob).

use android_firmware::{
    Filesystem, Name, RelativePath, STANDARD_SYSTEM, SuperLayout, VENDOR_SIDE, glob,
};
use recipe::{
    tree::{Expr, Tree},
    visit::Visitor,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};
use thiserror::Error;

/// The repository's manifest, at its root.
pub const MANIFEST: &str = "evelyn.toml";
/// A port's settings, in its directory.
pub const PORT: &str = "port.toml";
/// A patch's recipe, in its directory.
pub const PATCH: &str = "patch.ron";

#[derive(Debug, Error)]
pub enum Error {
    #[error("cannot read {path}: {source}")]
    Read {
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
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repository {
    pub rom: Name,
    pub device: Device,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
    pub name: Name,
    /// `ro.product.device` values the installer accepts.
    pub product_ids: Vec<String>,
    /// The stock firmware build the device's partitions must come from.
    pub stock: Stock,
    /// Settings for the device's own partitions, where the defaults are wrong.
    #[serde(default)]
    pub partitions: BTreeMap<Name, Settings>,
    #[serde(rename = "super", default)]
    pub super_layout: Option<SuperLayout>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stock {
    /// Each device partition's `ro.<partition>.build.fingerprint` must equal this.
    pub fingerprint: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Port {
    /// ROM partitions beyond the standard system side, and settings for any
    /// system-side partition. Keys are names or globs; the most specific wins.
    #[serde(default)]
    pub partitions: BTreeMap<String, Settings>,
    /// The patches this port applies, in order: directories under `patches/`.
    #[serde(default)]
    pub patches: Vec<RelativePath>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Default: the source image's filesystem.
    pub filesystem: Option<Filesystem>,
    /// Default: `/<name>`; for `system`, `/` or `/system` as its image is laid out.
    pub mount_point: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Profile {
    pub rom: Name,
    pub device: Device,
    /// The port's name: its directory under `ports/`.
    pub name: Name,
    pub port: Port,
    /// The repository root, where `evelyn.toml` is.
    pub root: PathBuf,
}

impl Profile {
    /// Load the port at `path`, its directory or its `port.toml`, and the
    /// repository it is in.
    pub fn load(path: &Path) -> Result<Self, Error> {
        let dir = if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(Path::new("."))
        };
        let dir = fs::canonicalize(dir).map_err(|source| Error::Read {
            path: dir.into(),
            source,
        })?;
        let not_a_port = || {
            Error::Invalid(format!(
                "{} is not a port: ports/<name> in a directory with {MANIFEST}",
                dir.display()
            ))
        };
        let ports = dir.parent().ok_or_else(not_a_port)?;
        let root = ports.parent().ok_or_else(not_a_port)?;
        if ports.file_name().is_none_or(|name| name != "ports") || !root.join(MANIFEST).is_file() {
            return Err(not_a_port());
        }
        let name = dir
            .file_name()
            .map(|name| Name::try_from(name.to_string_lossy().into_owned()))
            .ok_or_else(not_a_port)?
            .map_err(|error| Error::Invalid(error.to_string()))?;
        let repository: Repository = toml_file(&root.join(MANIFEST))?;
        let port: Port = toml_file(&dir.join(PORT))?;
        let profile = Self {
            rom: repository.rom,
            device: repository.device,
            name,
            port,
            root: root.to_path_buf(),
        };
        profile.validate()?;
        Ok(profile)
    }

    fn validate(&self) -> Result<(), Error> {
        if let Some(standard) = self
            .device
            .partitions
            .keys()
            .find(|name| STANDARD_SYSTEM.contains(&name.as_ref()))
        {
            return Err(Error::Invalid(format!(
                "device partition {standard} is system side, which comes from the ROM"
            )));
        }
        let settings = self
            .device
            .partitions
            .iter()
            .map(|(name, settings)| (name.as_ref(), settings));
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
    pub fn patches(&self) -> Vec<PathBuf> {
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
    pub fn settings(&self, partition: &str) -> Result<Option<&Settings>, Error> {
        if let Some((_, settings)) = self
            .device
            .partitions
            .iter()
            .find(|(name, _)| name.as_ref() == partition)
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
            .chain(self.device.partitions.keys().map(|name| name.as_ref()))
    }

    pub fn is_vendor_side(&self, partition: &str) -> bool {
        self.vendor_side().any(|vendor| vendor == partition)
    }

    /// `super` order for the device's partitions: Treble's order, then by name.
    pub fn device_order(partition: &Name) -> (usize, Name) {
        (
            VENDOR_SIDE
                .iter()
                .position(|vendor| *vendor == partition.as_ref())
                .unwrap_or(VENDOR_SIDE.len()),
            partition.clone(),
        )
    }

    /// A ROM partition this port builds into `super`.
    pub fn takes_from_rom(&self, partition: &str) -> bool {
        STANDARD_SYSTEM.contains(&partition)
            || (!self.is_vendor_side(partition)
                && self
                    .port
                    .partitions
                    .keys()
                    .any(|pattern| glob(pattern, partition)))
    }

    /// A partition name a recipe may refer to.
    pub fn known(&self, partition: &str) -> bool {
        self.is_vendor_side(partition) || self.takes_from_rom(partition)
    }

    /// Every partition `recipe` names must be one this port can have. Device paths
    /// are checked when they are resolved, against the build's mount points.
    pub fn validate_recipe(&self, tree: &Tree) -> Result<(), recipe::Error> {
        tree.walk(&mut Partitions(self))
    }
}

/// The load-time partition check, over the primitive tree.
struct Partitions<'a>(&'a Profile);

impl Partitions<'_> {
    fn known(&self, partition: &str) -> Result<(), recipe::Error> {
        if self.0.known(partition) {
            return Ok(());
        }
        Err(recipe::Error::Invalid(format!(
            "refers to partition {partition}, which this port neither defines nor takes by default"
        )))
    }
}

impl Visitor for Partitions<'_> {
    fn expr(
        &mut self,
        expr: &Expr,
        _bound: &[&recipe::tree::Binding],
    ) -> Result<(), recipe::Error> {
        match expr {
            Expr::HasPartition(name)
            | Expr::PropertyFiles {
                partition: Some(name),
            } => self.known(name.as_ref()),
            Expr::Select { scope, .. } => match &scope.partition {
                Some(partition) if !partition.contains(['*', '?']) => self.known(partition),
                _ => Ok(()),
            },
            _ => Ok(()),
        }
    }
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
