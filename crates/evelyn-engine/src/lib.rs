//! Firmware composition: device repositories, and the build pipeline that ports
//! a ROM to a device from them.

mod build;
mod firmware;
mod flashable;
mod fstab;
mod needs;
mod output;
pub mod profile;
mod properties;
pub use build::{Build, Built, Composed, build};
pub use firmware::{Extracted, Given, extract_in_place};
pub use profile::{Profile, init};

use android_firmware::{Filesystem, Name, SourceRef};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("source {0} is not EROFS or ext4")]
    SourceFilesystem(SourceRef),
    #[error(transparent)]
    Patch(#[from] patch::Error),
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
    #[error(transparent)]
    Android(#[from] android_fs::Error),
    #[error(transparent)]
    Toolchain(#[from] toolchain::Error),
    #[error("workspace IO: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Recipe(#[from] recipe::Error),
    #[error(transparent)]
    Profile(#[from] profile::Error),
    #[error(transparent)]
    Firmware(#[from] android_firmware::Error),
    #[error("step IDs must be unique across recipes: {0}")]
    DuplicateStep(Name),
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
    #[error("the stock firmware has no {0}.img, which evelyn.toml sets")]
    MissingStockPartition(Name),
    #[error(
        "stock {partition} is not the build evelyn.toml pins: expected {expected}, found {found}"
    )]
    StockFingerprint {
        partition: Name,
        expected: String,
        found: String,
    },
    #[error(
        "the system image has neither system/build.prop nor build.prop; set its mount_point in port.toml"
    )]
    SystemLayout,
    #[error("super and ZIP output need a [device.super] section in evelyn.toml")]
    MissingLayout,
    #[error("OTA package: {0}")]
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
}

/// What a build produces: partition images, a super image, or a flashable ZIP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    Partitions,
    Super,
    Zip,
}
