//! Device repositories and their ports.
//!
//! A device repository ports one ROM to one device. `evelyn.toml` at its root
//! describes the device: the product ID the installer accepts, the stock build
//! its partitions are pinned to, how partitions are built where the defaults are
//! wrong, and its `super` layout.
//! Each port, one ROM version, is `ports/<name>.toml`, which says which ROM builds
//! it is for, the ROM partitions it takes beyond the standard system side and, in
//! the order they apply, the patches it applies; a build takes the port the ROM it
//! is given matches. A patch is a directory directly under `patches/`, named for
//! what it changes, holding `patch.ron` and its assets; nothing applies unless a
//! port lists it. Neither file names a firmware build: the firmware is supplied
//! when building.
//!
//! Partitions follow the Treble split. The device's own partitions (the vendor side)
//! always come from the stock firmware, pinned by build fingerprint; the standard system side always comes from the ROM, and any other
//! ROM partition only when the port lists it (by name or glob).

use android::Filesystem;
use android::lp::SuperLayout;
use android::partition::{STANDARD_SYSTEM, VENDOR_SIDE};
use android::path::glob;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};
use thiserror::Error;

/// The repository's manifest, at its root.
const MANIFEST: &str = "evelyn.toml";
/// Where the ports are, each `<name>.toml`.
const PORTS: &str = "ports";
/// A patch's recipe, in its directory.
pub(crate) const PATCH: &str = "patch.ron";
/// What a repository keeps out of git: the work directory, the firmware and
/// what builds write.
const IGNORED: [&str; 3] = ["/.evelyn/", "/firmware/", "/out/"];

#[derive(Debug, Error)]
pub enum Error {
    #[error("cannot read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot write {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid profile {path}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("{MANIFEST} sets no product_id", MANIFEST = MANIFEST)]
    NoProductId,
    #[error("{MANIFEST} sets no fingerprint", MANIFEST = MANIFEST)]
    NoFingerprint,
    #[error("mount point for {0} must be absolute")]
    RelativeMountPoint(String),
    #[error("port partition {pattern} matches {vendor}, which comes from the stock firmware")]
    VendorSidePartition { pattern: String, vendor: String },
    #[error("patch {0:?} is not a directory name: a patch is patches/<name>")]
    NestedPatch(String),
    #[error("patch {0} is listed twice")]
    DuplicatePatch(String),
    #[error("patch {0} has no patches/{0}/{PATCH}", PATCH = PATCH)]
    MissingPatch(String),
    #[error("partition {partition} matches {a} and {b} in {table} equally, with different values")]
    AmbiguousSettings {
        table: &'static str,
        partition: String,
        a: String,
        b: String,
    },
    #[error("{0} is already a device repository")]
    AlreadyRepository(PathBuf),
    #[error("{0} is not in a device repository: no {MANIFEST} here or above", MANIFEST = MANIFEST)]
    NotInRepository(PathBuf),
}

/// `evelyn.toml`: the device a repository ports its ROM to.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Device {
    /// The `ro.product.device` value the installer accepts.
    pub product_id: String,
    /// Each device partition's `ro.<partition>.build.fingerprint` must equal this.
    pub fingerprint: String,
    /// Filesystems for partitions, device or ROM, where the source image's is
    /// wrong. Keys are names or globs; the most specific wins.
    #[serde(default)]
    pub filesystems: BTreeMap<String, Filesystem>,
    /// Mount points for partitions where the default is wrong, keyed the same way.
    #[serde(default)]
    pub mount_points: BTreeMap<String, String>,
    #[serde(rename = "super", default)]
    pub super_layout: Option<SuperLayout>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Port {
    /// The ROM builds this port is for: properties the ROM sets, each to a value
    /// its glob matches. A port without any is for every build.
    #[serde(default)]
    pub rom: BTreeMap<String, String>,
    /// The ROM partitions this port takes beyond the standard system side, by
    /// name or glob.
    #[serde(default)]
    pub partitions: Vec<String>,
    /// The patches this port applies, in order: directories directly under
    /// `patches/`, by name.
    #[serde(default)]
    pub patches: Vec<String>,
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

/// How a partition is built.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Settings {
    /// Default: the source image's filesystem.
    pub filesystem: Option<Filesystem>,
    /// Default: `/<name>`; for `system`, `/` or `/system` as its image is laid out.
    pub mount_point: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct Profile {
    pub device: Device,
    /// The port's name: its file name under `ports/`, without `.toml`.
    pub name: String,
    pub port: Port,
    /// The repository root, where `evelyn.toml` is.
    pub root: PathBuf,
}

impl Profile {
    /// Load the port `name` of the repository at `root`.
    pub(crate) fn load(root: &Path, name: &str) -> Result<Self, Error> {
        let device: Device = toml_file(&root.join(MANIFEST))?;
        let port: Port = toml_file(&root.join(PORTS).join(format!("{name}.toml")))?;
        let profile = Self {
            device,
            name: name.to_owned(),
            port,
            root: root.to_path_buf(),
        };
        profile.validate()?;
        Ok(profile)
    }

    fn validate(&self) -> Result<(), Error> {
        if self.device.product_id.is_empty() {
            return Err(Error::NoProductId);
        }
        if self.device.fingerprint.is_empty() {
            return Err(Error::NoFingerprint);
        }
        if let Some((pattern, _)) = self
            .device
            .mount_points
            .iter()
            .find(|(_, mount)| !mount.starts_with('/'))
        {
            return Err(Error::RelativeMountPoint(pattern.clone()));
        }
        for pattern in &self.port.partitions {
            if let Some(vendor) = VENDOR_SIDE.into_iter().find(|vendor| glob(pattern, vendor)) {
                return Err(Error::VendorSidePartition {
                    pattern: pattern.clone(),
                    vendor: vendor.to_owned(),
                });
            }
        }
        let mut listed = BTreeSet::new();
        for patch in &self.port.patches {
            if patch.is_empty() || patch == "." || patch == ".." || patch.contains(['/', '\\']) {
                return Err(Error::NestedPatch(patch.clone()));
            }
            if !listed.insert(patch) {
                return Err(Error::DuplicatePatch(patch.clone()));
            }
            if !self.root.join(Self::patch(patch)).is_file() {
                return Err(Error::MissingPatch(patch.clone()));
            }
        }
        Ok(())
    }

    /// The recipes of the patches this port applies, in the order they apply,
    /// by their paths in the repository.
    pub(crate) fn patches(&self) -> Vec<PathBuf> {
        self.port
            .patches
            .iter()
            .map(|patch| Self::patch(patch))
            .collect()
    }

    fn patch(patch: &str) -> PathBuf {
        Path::new("patches").join(patch).join(PATCH)
    }

    /// How `partition` is built: each setting from its most specific entry.
    pub(crate) fn settings(&self, partition: &str) -> Result<Settings, Error> {
        Ok(Settings {
            filesystem: most_specific(&self.device.filesystems, "filesystems", partition)?.copied(),
            mount_point: most_specific(&self.device.mount_points, "mount_points", partition)?
                .cloned(),
        })
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
            || (!VENDOR_SIDE.contains(&partition)
                && self
                    .port
                    .partitions
                    .iter()
                    .any(|pattern| glob(pattern, partition)))
    }
}

/// The value `table` sets for `partition` from its most specific entry: an exact
/// name beats any glob, and a glob with more literal characters beats one with
/// fewer.
fn most_specific<'a, T: PartialEq>(
    table: &'a BTreeMap<String, T>,
    name: &'static str,
    partition: &str,
) -> Result<Option<&'a T>, Error> {
    if let Some(value) = table.get(partition) {
        return Ok(Some(value));
    }
    let mut matches: Vec<_> = table
        .iter()
        .filter(|(pattern, _)| glob(pattern, partition))
        .map(|(pattern, value)| (literals(pattern), pattern, value))
        .collect();
    matches.sort_by_key(|(literals, ..)| std::cmp::Reverse(*literals));
    match matches.as_slice() {
        [] => Ok(None),
        [(first, a, value), (second, b, other), ..] if first == second && value != other => {
            Err(Error::AmbiguousSettings {
                table: name,
                partition: partition.to_owned(),
                a: a.to_string(),
                b: b.to_string(),
            })
        }
        [(.., value), ..] => Ok(Some(value)),
    }
}

/// A new device repository.
#[derive(Debug, Serialize)]
pub struct Initialized {
    pub root: PathBuf,
    /// The manifest, for the device's stock fingerprint, product ID and
    /// `super` layout to be filled in.
    pub manifest: PathBuf,
}

/// Set up a device repository at `root`: its manifest, the `ports/`, `patches/`
/// and `firmware/` directories, and what git ignores. One already there is left
/// alone.
pub fn init(root: &Path) -> Result<Initialized, Error> {
    let manifest = root.join(MANIFEST);
    if manifest.exists() {
        return Err(Error::AlreadyRepository(root.to_path_buf()));
    }
    let write = |path: &Path| {
        let path = path.to_path_buf();
        move |source| Error::Write { path, source }
    };
    let dirs = [PORTS, "patches", "firmware/stock", "firmware/rom"].map(|dir| root.join(dir));
    for dir in dirs {
        fs::create_dir_all(&dir).map_err(write(&dir))?;
    }
    let text = r#"# The ro.product.device value the installer accepts.
product_id = ""
# Each device partition's ro.<partition>.build.fingerprint must equal this.
fingerprint = ""

# The super partition, for super and ZIP output.
# [super]
# size = 0
# metadata_size = 65536
# slots = "virtual_ab"
# group = ""
"#;
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
        .ok_or_else(|| Error::NotInRepository(start.clone()))
}

/// The ports of the repository at `root`, by name.
pub(crate) fn ports(root: &Path) -> Result<BTreeMap<String, Port>, Error> {
    let dir = root.join(PORTS);
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
        if path.extension().is_none_or(|extension| extension != "toml") || !path.is_file() {
            continue;
        }
        let name = path
            .file_stem()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        ports.insert(name, toml_file(&path)?);
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

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICE: &str = r#"
        product_id = "Pong"
        fingerprint = "Nothing/phone2/phone2:17/A/B:user/release-keys"

        [filesystems]
        system = "ext4"
        "my_*" = "erofs"

        [mount_points]
        my_product = "/my_product"
    "#;

    fn profile(device: &str, port: &str) -> Result<Profile, Error> {
        let profile = Profile {
            device: toml::from_str(device).unwrap(),
            name: "17".to_owned(),
            port: toml::from_str(port).unwrap(),
            root: PathBuf::new(),
        };
        profile.validate()?;
        Ok(profile)
    }

    #[test]
    fn settings_come_from_the_manifest() {
        let profile = profile(DEVICE, r#"partitions = ["my_*"]"#).unwrap();
        assert_eq!(
            profile.settings("system").unwrap(),
            Settings {
                filesystem: Some(Filesystem::Ext4),
                mount_point: None
            }
        );
        assert_eq!(
            profile.settings("my_product").unwrap(),
            Settings {
                filesystem: Some(Filesystem::Erofs),
                mount_point: Some("/my_product".to_owned())
            }
        );
        assert_eq!(profile.settings("vendor").unwrap(), Settings::default());
    }

    #[test]
    fn equally_specific_globs_must_agree() {
        let device = DEVICE.replace(
            r#""my_*" = "erofs""#,
            r#""my_*" = "erofs"
        "*uct" = "ext4""#,
        );
        let profile = profile(&device, "").unwrap();
        assert!(matches!(
            profile.settings("my_product"),
            Err(Error::AmbiguousSettings {
                table: "filesystems",
                ..
            })
        ));
    }

    #[test]
    fn ports_take_rom_partitions_only() {
        let profile = profile(DEVICE, r#"partitions = ["my_*"]"#).unwrap();
        assert!(profile.takes_from_rom("system_ext"));
        assert!(profile.takes_from_rom("my_bigball"));
        assert!(!profile.takes_from_rom("odm"));
        assert!(matches!(
            super::tests::profile(DEVICE, r#"partitions = ["o*"]"#),
            Err(Error::VendorSidePartition { vendor, .. }) if vendor == "odm"
        ));
    }

    #[test]
    fn mount_points_are_absolute() {
        let relative = DEVICE.replace(r#""/my_product""#, r#""my_product""#);
        assert!(matches!(
            profile(&relative, ""),
            Err(Error::RelativeMountPoint(_))
        ));
    }

    #[test]
    fn the_old_manifest_is_refused() {
        let old = r#"
            rom = "OxygenOS"
            [device]
            name = "pong"
        "#;
        assert!(toml::from_str::<Device>(old).is_err());
    }
}
