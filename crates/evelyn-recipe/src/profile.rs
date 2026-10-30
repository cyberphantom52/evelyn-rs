//! Device and port profiles.
//!
//! A port lives at `devices/<device>/<rom>/<version>/`. `device.toml`, two levels up,
//! describes the device whatever ROM is ported to it; `port.toml` says what this
//! port takes from the ROM. Neither names a firmware build: the firmware is supplied
//! when building.
//!
//! Partitions follow the Treble split. The device's own partitions (the vendor side
//! the stock firmware supplies) always come from it, pinned by build fingerprint;
//! the standard system side always comes from the ROM, and any other ROM partition
//! only when `port.toml` lists it (by name or glob).

use crate::{
    Error, Recipe, read,
    tree::{Expr, Stmt},
    visit::Visitor,
};
use android_firmware::{Filesystem, Name};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

/// Treble's vendor side, in the order partitions are laid out in `super`. A device
/// lists the ones it has; recipes may name any of them, for conditions.
pub const VENDOR_SIDE: [&str; 4] = ["odm", "vendor", "vendor_dlkm", "odm_dlkm"];
/// System-side partitions every port takes from the ROM, in `super` order.
pub const STANDARD_SYSTEM: [&str; 4] = ["system", "system_ext", "product", "system_dlkm"];
/// The firmware a partition or import comes from.
pub const ROM: &str = "rom";
pub const STOCK: &str = "stock";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
    pub schema: u32,
    pub device: Name,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuperLayout {
    pub size: u64,
    pub metadata_size: u32,
    pub slots: SlotScheme,
    /// The one dynamic partition group, named as the vendor names it.
    pub group: Name,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotScheme {
    Single,
    Ab,
    VirtualAb,
}

impl SuperLayout {
    pub fn metadata_slots(&self) -> u32 {
        match self.slots {
            SlotScheme::Single => 2,
            SlotScheme::Ab | SlotScheme::VirtualAb => 3,
        }
    }

    /// A slot's group may use the whole device, except on A/B, where both slots
    /// hold full copies.
    pub fn group_capacity(&self) -> u64 {
        match self.slots {
            SlotScheme::Ab => self.size / 2,
            SlotScheme::Single | SlotScheme::VirtualAb => self.size,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Port {
    pub schema: u32,
    /// ROM partitions beyond the standard system side, and settings for any
    /// system-side partition. Keys are names or globs; the most specific wins.
    #[serde(default)]
    pub partitions: BTreeMap<String, Settings>,
    /// Discovered recipes this port does not apply, relative to the repository root.
    #[serde(default)]
    pub exclude: Vec<PathBuf>,
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
    pub device: Device,
    pub port: Port,
    pub rom: String,
    pub version: String,
    /// The port directory, `devices/<device>/<rom>/<version>`.
    pub dir: PathBuf,
    /// The repository root, four levels above the port directory.
    pub root: PathBuf,
}

impl Profile {
    /// Load the port at `path`, its directory or its `port.toml`.
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
        let component = |path: &Path| -> Result<String, Error> {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .ok_or_else(|| {
                    Error::Invalid(format!(
                        "{} is not a devices/<device>/<rom>/<version> directory",
                        dir.display()
                    ))
                })
        };
        let rom_dir = dir.parent().unwrap_or(&dir);
        let device_dir = rom_dir.parent().unwrap_or(rom_dir);
        let root = device_dir
            .parent()
            .and_then(Path::parent)
            .unwrap_or(device_dir)
            .to_path_buf();
        let device: Device = toml_file(&device_dir.join("device.toml"))?;
        let port: Port = toml_file(&dir.join("port.toml"))?;
        let profile = Self {
            rom: component(rom_dir)?,
            version: component(&dir)?,
            device,
            port,
            root,
            dir,
        };
        profile.validate()?;
        Ok(profile)
    }

    fn validate(&self) -> Result<(), Error> {
        if self.device.schema != 1 || self.port.schema != 1 {
            return Err(Error::Invalid("unsupported profile schema".into()));
        }
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
                    "port partition {pattern} matches {vendor}, which comes from the stock firmware; set it in device.toml"
                )));
            }
        }
        let discovered = self.discovered()?;
        for excluded in &self.port.exclude {
            if !discovered.contains(&self.root.join(excluded)) {
                return Err(Error::Invalid(format!(
                    "excluded recipe {} is not one this port would apply",
                    excluded.display()
                )));
            }
        }
        Ok(())
    }

    /// The recipes this port applies, general to specific: `roms/common`, then
    /// `roms/<rom>/<version>/common`, then the port directory, each in file name order.
    pub fn recipes(&self) -> Result<Vec<PathBuf>, Error> {
        let excluded: Vec<_> = self
            .port
            .exclude
            .iter()
            .map(|path| self.root.join(path))
            .collect();
        Ok(self
            .discovered()?
            .into_iter()
            .filter(|path| !excluded.contains(path))
            .collect())
    }

    fn discovered(&self) -> Result<Vec<PathBuf>, Error> {
        let roms = self.root.join("roms");
        let layers = [
            roms.join("common"),
            roms.join(&self.rom).join(&self.version).join("common"),
            self.dir.clone(),
        ];
        let mut recipes = Vec::new();
        for layer in layers {
            let Ok(entries) = fs::read_dir(&layer) else {
                continue;
            };
            let mut found: Vec<_> = entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.extension().is_some_and(|ext| ext == "ron"))
                .collect();
            found.sort();
            recipes.extend(found);
        }
        Ok(recipes)
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
    pub fn validate_recipe(&self, recipe: &Recipe) -> Result<(), Error> {
        recipe.tree()?.walk(&mut Partitions {
            profile: self,
            step: None,
        })
    }
}

/// The load-time partition check, over the primitive tree.
struct Partitions<'a> {
    profile: &'a Profile,
    step: Option<Name>,
}

impl Partitions<'_> {
    fn known(&self, partition: &str) -> Result<(), Error> {
        if self.profile.known(partition) {
            return Ok(());
        }
        Err(Error::Invalid(format!(
            "step {} refers to partition {partition}, which this port neither defines nor takes by default",
            self.step.as_ref().map_or("?", |step| step.as_ref())
        )))
    }
}

impl Visitor for Partitions<'_> {
    fn stmt(&mut self, stmt: &Stmt, _bound: &[&Name]) -> Result<(), Error> {
        match stmt {
            Stmt::Step { id, .. } => {
                self.step = Some(id.clone());
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn expr(&mut self, expr: &Expr, _bound: &[&Name]) -> Result<(), Error> {
        let target = match expr {
            Expr::Partition(name) => return self.known(name.as_ref()),
            Expr::Select { scope, .. } => {
                return match &scope.partition {
                    Some(partition) if !partition.contains(['*', '?']) => self.known(partition),
                    _ => Ok(()),
                };
            }
            Expr::Prop { target, .. }
            | Expr::PropertyFiles { target, .. }
            | Expr::NewPropertyFile(target) => target,
            _ => return Ok(()),
        };
        match target.as_ref().and_then(crate::PropertyTarget::partition) {
            Some(partition) => self.known(partition.as_ref()),
            None => Ok(()),
        }
    }
}

/// The mount point a partition gets unless the port overrides it: `/<name>`, except
/// `system`, which depends on the image (see `system_mount_point`).
pub fn default_mount_point(partition: &str) -> Option<String> {
    (partition != "system").then(|| format!("/{partition}"))
}

/// `/` for a system-as-root `system` image, whose top level is the device's root
/// with `/system` beneath it, or `/system` for an older one, whose top level is
/// `/system` itself.
pub fn system_mount_point(has: impl Fn(&str) -> bool) -> Option<&'static str> {
    if has("system/build.prop") {
        Some("/")
    } else if has("build.prop") {
        Some("/system")
    } else {
        None
    }
}

fn toml_file<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, Error> {
    toml::from_str(&read(path)?).map_err(|source| Error::Toml {
        path: path.into(),
        source,
    })
}

/// `*` matches any run of characters and `?` any one.
fn glob(pattern: &str, name: &str) -> bool {
    fn matches(pattern: &[u8], name: &[u8]) -> bool {
        match pattern.split_first() {
            None => name.is_empty(),
            Some((b'*', rest)) => (0..=name.len()).any(|skip| matches(rest, &name[skip..])),
            Some((b'?', rest)) => !name.is_empty() && matches(rest, &name[1..]),
            Some((byte, rest)) => name.first() == Some(byte) && matches(rest, &name[1..]),
        }
    }
    matches(pattern.as_bytes(), name.as_bytes())
}

fn literals(pattern: &str) -> usize {
    pattern.chars().filter(|c| !matches!(c, '*' | '?')).count()
}
