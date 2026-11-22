//! ART's compiled artifacts as a build ships them next to a JAR or APK
//! (`oat/<isa>/<name>.{odex,vdex,art}`): what an odex's oat header records of how
//! it was compiled, and the fs-verity metadata (`.fsv_meta`) the build writes
//! beside each file.

use crate::Error;
use crate::elf::{dynamic_symbol, int};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// An odex's oat header.
pub struct Oat {
    /// ART's `InstructionSet`: 2 is arm64.
    pub instruction_set: u32,
    /// ART's bitmap of the instruction set features the code may use.
    pub features: u32,
    /// The key-value store: the compiler filter, the boot class path and its
    /// checksums, the class loader context, and more.
    pub store: BTreeMap<String, String>,
}

/// ART's `InstructionSet` of arm64.
const ARM64: u32 = 2;
/// The fields of the oat header after its magic and version, the last of them
/// the key-value store's size, which the store follows.
const FIELDS: usize = 16;

impl Oat {
    /// The oat header of an odex, at its `oatdata` symbol: `odex` may be just the
    /// start of the file, up to the end of the header.
    pub fn read(odex: &[u8]) -> Result<Self, Error> {
        let at =
            dynamic_symbol(odex, "oatdata")?.ok_or_else(|| unsupported("no oatdata symbol"))?;
        if odex.get(at..at + 4) != Some(b"oat\n") {
            return Err(unsupported("no oat header at oatdata"));
        }
        let field = |i: usize| int(odex, at + 8 + 4 * i, 4).map(|value| value as u32);
        let start = at + 8 + 4 * FIELDS;
        let store = odex
            .get(start..start + field(FIELDS - 1)? as usize)
            .ok_or_else(|| unsupported("truncated oat key-value store"))?;
        // Keys and values are NUL-terminated; a value may be padded with NULs,
        // which read as empty keys.
        let mut strings = store
            .split(|b| *b == 0)
            .map(|s| String::from_utf8_lossy(s).into_owned());
        let mut pairs = BTreeMap::new();
        while let Some(key) = strings.next() {
            if !key.is_empty() {
                pairs.insert(key, strings.next().unwrap_or_default());
            }
        }
        Ok(Self {
            instruction_set: field(1)?,
            features: field(2)?,
            store: pairs,
        })
    }

    pub fn get(&self, key: &str) -> Result<&str, Error> {
        self.store
            .get(key)
            .map(String::as_str)
            .ok_or_else(|| unsupported(&format!("no {key} in the oat header")))
    }

    /// The instruction set, as dex2oat names it.
    pub fn isa(&self) -> Result<&'static str, Error> {
        match self.instruction_set {
            ARM64 => Ok("arm64"),
            other => Err(unsupported(&format!("instruction set {other}"))),
        }
    }

    /// The `--instruction-set-features` list the feature bitmap stands for, each
    /// feature on or off (`-`).
    pub fn feature_list(&self) -> Result<String, Error> {
        if self.instruction_set != ARM64 {
            return Err(unsupported(
                "features of an instruction set other than arm64",
            ));
        }
        let names = ["a53", "crc", "lse", "fp16", "dotprod", "sve"];
        let list: Vec<String> = names
            .iter()
            .enumerate()
            .map(|(bit, name)| match self.features & 1 << bit {
                0 => format!("-{name}"),
                _ => (*name).to_owned(),
            })
            .collect();
        Ok(list.join(","))
    }
}

/// The boot image components `bootclasspath-checksums` records, `i;<jars>/…`
/// each, in order: how many boot class path jars each covers. A component
/// that is not an image (`d;…`, the jars' own checksums) is refused.
pub fn boot_image_components(checksums: &str) -> Result<Vec<usize>, Error> {
    checksums
        .split(':')
        .map(|component| {
            component
                .strip_prefix("i;")
                .and_then(|rest| rest.split('/').next())
                .and_then(|count| count.parse().ok())
                .ok_or_else(|| unsupported(&format!("boot class path checksum {component}")))
        })
        .collect()
}

/// fs-verity's block size, which the build's metadata uses.
const BLOCK: usize = 4096;

/// The fs-verity metadata the build writes as `<file>.fsv_meta`: a version, the
/// fs-verity descriptor of `data` (SHA-256, 4 KiB blocks, no salt), no signature,
/// then, for data of more than one block, its Merkle tree from the root level
/// down, from the next block on.
pub fn fsverity_metadata(data: &[u8]) -> Vec<u8> {
    let hash = |block: &[u8]| {
        let mut padded = block.to_vec();
        padded.resize(BLOCK, 0);
        Sha256::digest(&padded).to_vec()
    };
    let mut levels: Vec<Vec<u8>> = Vec::new();
    let root = match data.len() <= BLOCK {
        true => hash(data),
        false => {
            let mut level = data.to_vec();
            loop {
                let mut hashes: Vec<u8> = level.chunks(BLOCK).flat_map(hash).collect();
                hashes.resize(hashes.len().div_ceil(BLOCK) * BLOCK, 0);
                if hashes.len() == BLOCK {
                    let root = hash(&hashes);
                    levels.push(hashes);
                    break root;
                }
                levels.push(hashes.clone());
                level = hashes;
            }
        }
    };
    let mut out = 1u32.to_le_bytes().to_vec();
    out.extend([1, 1, 12, 0]); // version, SHA-256, log2 of the block size, salt size
    out.extend(0u32.to_le_bytes()); // signature size
    out.extend((data.len() as u64).to_le_bytes());
    out.extend(&root);
    out.resize(4 + 256, 0); // root hash to 64 bytes, salt, reserved
    out.extend(0u32.to_le_bytes()); // no signature
    out.extend(0u32.to_le_bytes());
    if !levels.is_empty() {
        out.resize(BLOCK, 0);
        for level in levels.iter().rev() {
            out.extend(level);
        }
    }
    out
}

fn unsupported(what: &str) -> Error {
    Error::Unsupported(what.to_owned())
}
