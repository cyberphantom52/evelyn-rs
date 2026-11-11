//! Android firmware facts and addressing: partition names and paths, the device paths
//! partitions are mounted under, which side of Treble a partition is on, the `super`
//! layout, the metadata Android gives a file, and content hashes.

use crate::path::Name;
use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid {kind}: {value:?}")]
    Invalid { kind: &'static str, value: String },
}

/// Whether `text` is a non-empty run of `[A-Za-z0-9._-]`: safe in a path component
/// or a recipe, for a value read from firmware.
pub fn is_token(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// The two firmwares a port is built from: the ROM being ported, which supplies the
/// system side, and the device's stock firmware, which supplies the vendor side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Firmware {
    Rom,
    Stock,
}

impl fmt::Display for Firmware {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Rom => "rom",
            Self::Stock => "stock",
        })
    }
}

/// A partition image of one of the port's firmwares, written `firmware.partition`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SourceRef {
    pub source: Firmware,
    pub partition: Name,
}

impl fmt::Display for SourceRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.source, self.partition)
    }
}
