//! Where a recipe names a file, a directory or a partition. A path can name a
//! bound value as `{name}`, and one that does is checked when it is used.

use crate::Error;
use android_firmware::{DevicePath, Name, RelativePath};
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

/// The kinds of location a field takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Build,
    Rom,
    Stock,
    Asset,
    Package,
    Partition,
}

impl Location {
    pub(crate) fn kind(&self) -> Kind {
        match self {
            Self::Build(_) => Kind::Build,
            Self::Rom(_) => Kind::Rom,
            Self::Stock(_) => Kind::Stock,
            Self::Asset(_) => Kind::Asset,
            Self::Package(_) => Kind::Package,
            Self::Partition(_) => Kind::Partition,
        }
    }

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

    /// That it is a kind `what` takes, and, written out, a well-formed path: a
    /// device path in a firmware, one relative to the recipe's directory for an
    /// asset, and one from the root inside a package.
    pub(crate) fn check(&self, allowed: &[Kind], what: &str) -> Result<(), Error> {
        if !allowed.contains(&self.kind()) {
            return Err(Error::Invalid(format!(
                "{what} takes {allowed:?}, not {:?}({:?})",
                self.kind(),
                self.path()
            )));
        }
        let Some(path) = crate::expr::written_out(self.path()) else {
            return Ok(());
        };
        let path = path.as_str();
        match self {
            Self::Rom(_) | Self::Stock(_) | Self::Package(_) => {
                DevicePath::try_from(path.to_owned()).map(drop)?
            }
            Self::Asset(_) => RelativePath::try_from(path.to_owned()).map(drop)?,
            Self::Build(_) if path.starts_with('/') => {
                DevicePath::try_from(path.to_owned()).map(drop)?
            }
            Self::Build(_) => RelativePath::try_from(path.to_owned()).map(drop)?,
            Self::Partition(_) => {}
        }
        Ok(())
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.path())
    }
}
