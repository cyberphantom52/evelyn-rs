//! What the firmware's property files set: the port the ROM matches, and whether
//! the stock partitions are the build `evelyn.toml` pins.

use crate::{Error, firmware::Firmware, profile};
use android::firmware::{Name, STANDARD_SYSTEM};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

/// A stock partition must be the build `evelyn.toml` pins: the property files init
/// loads from it set `ro.<partition>.build.fingerprint` to `expected`.
pub(crate) fn check_fingerprint(
    partition: &Name,
    image: &Path,
    expected: &str,
) -> Result<(), Error> {
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
pub(crate) fn rom_properties(rom: &Firmware) -> Result<BTreeMap<String, String>, Error> {
    let mut properties = BTreeMap::new();
    for name in STANDARD_SYSTEM {
        if let Some(partition) = rom.names().find(|key| key.as_ref() == name)
            && let Some(image) = rom.image(partition)
        {
            properties.extend(loaded(partition, image)?);
        }
    }
    Ok(properties)
}

/// The one port of the repository at `root` a ROM setting `properties` matches.
pub(crate) fn port_for(root: &Path, properties: &BTreeMap<String, String>) -> Result<Name, Error> {
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
    let groups: Vec<Vec<&str>> = android::firmware::PROPERTY_FILES
        .iter()
        .map(|group| {
            group
                .iter()
                .filter_map(|path| path.strip_prefix(mount.as_str()))
                .collect::<Vec<_>>()
        })
        .filter(|group| !group.is_empty())
        .collect();
    let read: BTreeMap<String, Vec<u8>> = android::Image::detect(image)?
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
            for entry in android::property::entries(&String::from_utf8_lossy(bytes)) {
                assigned.insert(entry.key.to_owned(), entry.value.to_owned());
            }
        }
    }
    Ok(assigned)
}
