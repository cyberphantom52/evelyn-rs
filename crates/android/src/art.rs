//! ART's compiled artifacts as a build ships them: what an oat file's header
//! records of how it was compiled (an odex beside a JAR or APK, or a boot image's
//! `boot*.oat`), what a boot image's `.art` header records of its layout, and the
//! fs-verity metadata (`.fsv_meta`) the build writes beside each file.
//!
//! The layouts are those of the ART release a ROM ships, from AOSP's sources at
//! `android-17.0.0_r1` (oat version 275, image version 119):
//! - the oat header: `art/runtime/oat/oat.h`, `OatHeader`'s fields and keys;
//! - the arm64 feature bitmap: `art/runtime/arch/arm64/instruction_set_features_arm64.{h,cc}`,
//!   `AsBitmap` and `GetFeatureString`;
//! - `InstructionSet`: `art/libartbase/arch/instruction_set.h`;
//! - the image header: `art/runtime/oat/image.h`, `ImageHeader`'s fields;
//! - the fs-verity metadata: the platform build's `fsverity_metadata_generator`,
//!   checked byte for byte against a ROM's.

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
/// For each oat version Evelyn knows, how many 32-bit fields follow the header's
/// magic and version, the last of them the key-value store's size, which the
/// store follows.
const OAT_VERSIONS: [(&[u8; 4], usize); 1] = [(b"275\0", 16)];
/// The arm64 features in the order of their bits.
const ARM64_FEATURES: [&str; 6] = ["a53", "crc", "lse", "fp16", "dotprod", "sve"];

impl Oat {
    /// The oat header of an odex, at its `oatdata` symbol: `odex` may be just the
    /// start of the file, up to the end of the header.
    pub fn read(odex: &[u8]) -> Result<Self, Error> {
        let at =
            dynamic_symbol(odex, "oatdata")?.ok_or_else(|| unsupported("no oatdata symbol"))?;
        if odex.get(at..at + 4) != Some(b"oat\n") {
            return Err(unsupported("no oat header at oatdata"));
        }
        let version = odex.get(at + 4..at + 8).unwrap_or_default();
        let fields = OAT_VERSIONS
            .iter()
            .find(|(known, _)| known.as_slice() == version)
            .map(|(_, fields)| *fields)
            .ok_or_else(|| {
                unsupported(&format!(
                    "oat version {}",
                    String::from_utf8_lossy(version).trim_end_matches('\0')
                ))
            })?;
        let field = |i: usize| int(odex, at + 8 + 4 * i, 4).map(|value| value as u32);
        let start = at + 8 + 4 * fields;
        let store = odex
            .get(start..start + field(fields - 1)? as usize)
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
        if self.features >> ARM64_FEATURES.len() != 0 {
            return Err(unsupported(&format!(
                "arm64 feature bitmap {:#x}",
                self.features
            )));
        }
        let list: Vec<String> = ARM64_FEATURES
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

/// What a boot image's `.art` file records of where it is and what it covers.
pub struct ImageHeader {
    /// For the first file of a boot image or extension, how many boot class path
    /// jars it covers; 0 for the others.
    pub component_count: u32,
    /// The address it is mapped at, which the first image file is compiled for.
    pub image_begin: u32,
    /// Its checksum, which, for the first file, an oat file compiled against the
    /// image records in `bootclasspath-checksums`.
    pub image_checksum: u32,
    /// For an extension, how many boot class path jars the images it extends cover.
    pub boot_image_component_count: u32,
}

impl ImageHeader {
    /// The header at the start of an `.art` file, version 119.
    pub fn read(art: &[u8]) -> Result<Self, Error> {
        if art.get(..8) != Some(b"art\n119\0") {
            return Err(unsupported("an image that is not version 119"));
        }
        // After the magic and version: image_reservation_size_, component_count_,
        // image_begin_, image_size_, image_checksum_, … boot_image_component_count_
        // (the 13th field).
        let field = |i: usize| int(art, 8 + 4 * i, 4).map(|value| value as u32);
        Ok(Self {
            component_count: field(1)?,
            image_begin: field(2)?,
            image_checksum: field(4)?,
            boot_image_component_count: field(12)?,
        })
    }

    /// This image as an oat file compiled against it records it in
    /// `bootclasspath-checksums`.
    pub fn checksum(&self) -> String {
        format!("i;{}/{:08x}", self.component_count, self.image_checksum)
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
