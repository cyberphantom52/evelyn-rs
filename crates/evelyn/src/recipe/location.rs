//! Where a recipe names a file, a directory or a partition. A path can name a
//! bound value as `{name}`.

use android::path::Name;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Location {
    /// What a pattern matches in the build: a device path, globs allowed, or a
    /// relative one, tried in the directory of every partition.
    Build(String),
    /// What a device path, globs allowed, matches in the ROM.
    Rom(String),
    /// What a device path, globs allowed, matches in the stock firmware.
    Stock(String),
    /// A file or directory in the recipe's directory, by its path there.
    Asset(String),
    /// What a path, from the root of the package a `Package` has open, matches.
    Package(String),
    /// A partition of the build.
    Partition(Name),
}

impl Location {
    /// The path or pattern, or the partition's name.
    pub fn path(&self) -> &str {
        match self {
            Self::Build(path)
            | Self::Rom(path)
            | Self::Stock(path)
            | Self::Asset(path)
            | Self::Package(path) => path,
            Self::Partition(name) => name.as_ref(),
        }
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.path())
    }
}
