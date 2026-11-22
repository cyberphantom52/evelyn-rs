//! APEX files: the name their manifest gives them, and their payload, the
//! filesystem image the device mounts at `/apex/<name>`. A compressed APEX
//! (`.capex`) holds the APEX itself as `original_apex`.

use crate::Error;
use std::io::{Cursor, Read};
use zip::ZipArchive;

/// The name `apex_manifest.pb` gives the APEX (or compressed APEX) `apex`.
pub fn name(apex: &[u8]) -> Result<String, Error> {
    let manifest = entry(apex, "apex_manifest.pb")?;
    // ApexManifest's field 1, `name`, a string.
    let mut at = 0;
    while at < manifest.len() {
        let key = varint(&manifest, &mut at)?;
        let value = match key & 7 {
            0 => {
                varint(&manifest, &mut at)?;
                continue;
            }
            1 => 8,
            2 => varint(&manifest, &mut at)? as usize,
            5 => 4,
            _ => return Err(unsupported("apex_manifest.pb")),
        };
        let field = manifest
            .get(at..at + value)
            .ok_or_else(|| unsupported("truncated apex_manifest.pb"))?;
        if key == 1 << 3 | 2 {
            return Ok(String::from_utf8_lossy(field).into_owned());
        }
        at += value;
    }
    Err(unsupported("an apex_manifest.pb without a name"))
}

/// The payload image of the APEX (or compressed APEX) `apex`.
pub fn payload(apex: &[u8]) -> Result<Vec<u8>, Error> {
    match entry(apex, "original_apex") {
        Ok(original) => entry(&original, "apex_payload.img"),
        Err(_) => entry(apex, "apex_payload.img"),
    }
}

fn entry(zip: &[u8], name: &str) -> Result<Vec<u8>, Error> {
    let mut archive =
        ZipArchive::new(Cursor::new(zip)).map_err(|_| unsupported("an APEX that is not a ZIP"))?;
    let mut file = archive
        .by_name(name)
        .map_err(|_| unsupported(&format!("an APEX without {name}")))?;
    let mut data = Vec::with_capacity(file.size() as usize);
    file.read_to_end(&mut data).map_err(|source| Error::Entry {
        path: name.to_owned(),
        source,
    })?;
    Ok(data)
}

fn varint(data: &[u8], at: &mut usize) -> Result<u64, Error> {
    let mut value = 0;
    for shift in (0..64).step_by(7) {
        let byte = *data
            .get(*at)
            .ok_or_else(|| unsupported("truncated apex_manifest.pb"))?;
        *at += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(unsupported("apex_manifest.pb"))
}

fn unsupported(what: &str) -> Error {
    Error::Unsupported(what.to_owned())
}
