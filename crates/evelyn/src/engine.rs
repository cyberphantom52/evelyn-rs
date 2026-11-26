//! Firmware composition: device repositories, and the build pipeline that ports
//! a ROM to a device from them.

mod build;
mod derived;
mod device;
mod dexopt;
mod firmware;
mod flashable;
mod fstab;
mod needs;
mod output;
pub mod profile;
mod properties;
pub use build::{Build, Built, Composed, build};
pub use firmware::{Extracted, Given, extract_in_place};
use profile::Profile;
pub use profile::init;

use crate::SourceRef;
use android::Filesystem;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// An I/O result with the path it was about attached to its error.
pub(crate) trait IoAt<T> {
    fn at(self, path: impl AsRef<Path>) -> Result<T, Error>;
}

impl<T> IoAt<T> for std::io::Result<T> {
    fn at(self, path: impl AsRef<Path>) -> Result<T, Error> {
        self.map_err(|source| Error::Io {
            path: path.as_ref().to_path_buf(),
            source,
        })
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("source {0} is not EROFS or ext4")]
    SourceFilesystem(SourceRef),
    #[error(transparent)]
    Patch(#[from] crate::patch::Error),
    #[error("cannot apply {recipe}")]
    Apply {
        recipe: PathBuf,
        #[source]
        source: crate::patch::Error,
    },
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
    #[error(transparent)]
    Android(#[from] android::Error),
    #[error("cannot access {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Recipe(#[from] crate::recipe::Error),
    #[error(transparent)]
    Profile(#[from] profile::Error),
    #[error("{fstab} mounts {partition}, which this build does not have")]
    FstabPartition { fstab: String, partition: String },
    #[error("{fstab} mounts {partition} as {accepted:?}, but it is built as {filesystem}")]
    FstabFilesystem {
        fstab: String,
        partition: String,
        filesystem: Filesystem,
        accepted: Vec<String>,
    },
    #[error("ZIP output flashes the stock firmware's images from {0}, which does not exist")]
    MissingFirmware(PathBuf),
    #[error(
        "stock {partition} is not the build evelyn.toml pins: expected {expected}, found {found}"
    )]
    StockFingerprint {
        partition: String,
        expected: String,
        found: String,
    },
    #[error(
        "the system image has neither system/build.prop nor build.prop; set mount_points.system in evelyn.toml"
    )]
    SystemLayout,
    #[error("super and ZIP output need a [super] section in evelyn.toml")]
    MissingLayout,
    #[error("cannot start the threads that extract the OTA package")]
    Runtime(#[source] std::io::Error),
    #[error("cannot read the OTA package")]
    Payload(#[from] payload_dumper::Error),
    #[error("{0} is an incremental OTA; give a full one")]
    IncrementalOta(String),
    #[error("{image}, extracted from {package}, does not match the hash its manifest records")]
    OtaHash { package: String, image: PathBuf },
    #[error("{0} holds more than one OTA package")]
    AmbiguousFirmware(PathBuf),
    #[error("{0} holds both an OTA package and images: keep one")]
    MixedFirmware(PathBuf),
    #[error("the ROM matches no port ({0})")]
    NoPort(String),
    #[error("the ROM matches more than one port: {0}")]
    Ports(String),
    #[error(transparent)]
    Tool(#[from] crate::toolchain::Error),
    #[error("the build has no APEX named {0}")]
    NoApex(String),
    #[error("the build has no boot image {0}")]
    NoBootImage(String),
    #[error("cannot compile the odex of {dex} again")]
    Dexopt {
        dex: String,
        #[source]
        source: Box<Error>,
    },
    #[error("cannot run {0}")]
    ArtRun(PathBuf, #[source] std::io::Error),
    #[error("{0} exited with {1}; it logs only to logd, which the host lacks")]
    ArtFailed(&'static str, std::process::ExitStatus),
    #[error("{0} exited with {1}: {2}")]
    ArtSaid(&'static str, std::process::ExitStatus, String),
    #[error("the new odex records {key} {built:?}, the ROM's {stock:?}")]
    ArtDiffers {
        key: &'static str,
        stock: String,
        built: String,
    },
}

/// What a build produces: partition images, a super image, or a flashable ZIP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    Partitions,
    Super,
    Zip,
}
