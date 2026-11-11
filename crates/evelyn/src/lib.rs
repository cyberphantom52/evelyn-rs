//! Evelyn: ports a ROM to a device. The recipe language, applying recipes to
//! partition trees, the build pipeline over device repositories, and the files
//! Evelyn downloads.

pub mod engine;
pub mod patch;
pub mod recipe;
pub mod toolchain;

use android::path::Name;
use serde::{Deserialize, Serialize};
use std::fmt;

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
