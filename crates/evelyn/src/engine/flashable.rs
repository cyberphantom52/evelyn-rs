//! Recovery-flashable ZIPs: the installer template, the zstd-compressed super image,
//! and any device firmware images, written in one streaming pass.

use crate::engine::{Error, IoAt};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const UPDATE_BINARY: &str = "META-INF/com/google/android/update-binary";

/// The installer files, with `INSERT_DEVICE_ID` in update-binary.
const TEMPLATE: [(&str, &[u8]); 3] = [
    (
        UPDATE_BINARY,
        include_bytes!("../../../../assets/flashable/META-INF/com/google/android/update-binary"),
    ),
    (
        "META-INF/com/google/android/updater-script",
        include_bytes!("../../../../assets/flashable/META-INF/com/google/android/updater-script"),
    ),
    (
        "META-INF/zstd",
        include_bytes!("../../../../assets/flashable/META-INF/zstd"),
    ),
];

pub(crate) struct Contents<'a> {
    pub device: &'a str,
    pub super_image: &'a Path,
    pub firmware: &'a [PathBuf],
}

pub(crate) fn write(contents: &Contents<'_>, output: &Path) -> Result<(), Error> {
    let mut zip = ZipWriter::new(fs::File::create(output).at(output)?);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .unix_permissions(0o644);
    for (name, bytes) in TEMPLATE {
        zip.start_file(name, options)?;
        if name == UPDATE_BINARY {
            let text = String::from_utf8_lossy(bytes).replace("INSERT_DEVICE_ID", contents.device);
            zip.write_all(text.as_bytes()).at(output)?;
        } else {
            zip.write_all(bytes).at(output)?;
        }
    }
    compress(&mut zip, output, contents.super_image, "super.img.zst")?;
    let mut names = String::new();
    for image in contents.firmware {
        let name = image.file_stem().unwrap_or_default().to_string_lossy();
        compress(
            &mut zip,
            output,
            image,
            &format!("firmware-update/{name}.img.zst"),
        )?;
        names.push_str(&name);
        names.push('\n');
    }
    if !contents.firmware.is_empty() {
        zip.start_file("firmware-update/partition_list.txt", options)?;
        zip.write_all(names.as_bytes()).at(output)?;
    }
    zip.finish()?;
    Ok(())
}

/// Stream `image` through a zstd encoder straight into a new ZIP member of the
/// ZIP at `output`.
fn compress(
    zip: &mut ZipWriter<fs::File>,
    output: &Path,
    image: &Path,
    name: &str,
) -> Result<(), Error> {
    tracing::info!(image = %image.display(), "compressing into ZIP");
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .unix_permissions(0o644)
        .large_file(true);
    zip.start_file(name, options)?;
    let mut encoder = zstd::stream::write::Encoder::new(&mut *zip, 9).at(output)?;
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    encoder.multithread(threads).at(output)?;
    let mut input = fs::File::open(image).at(image)?;
    io::copy(&mut input, &mut encoder).at(output)?;
    encoder.finish().at(output)?;
    Ok(())
}
