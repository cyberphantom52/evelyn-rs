//! Align a package's stored entries, as `zipalign -P 16 4` does.

use std::{
    fs,
    io::{self, BufReader},
    path::Path,
};
use thiserror::Error;
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

#[derive(Debug, Error)]
pub enum Error {
    #[error("package ZIP: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("package IO: {0}")]
    Io(#[from] io::Error),
    #[error("stored entry {0} could not be aligned")]
    Unaligned(String),
}

/// Stored data starts on a 4-byte boundary, and a stored shared library on a 16 KiB
/// page, so that Android can map it in place.
fn alignment(name: &str) -> u64 {
    if name.ends_with(".so") { 16 << 10 } else { 4 }
}

/// Copy `input` to `output` with every stored entry's data aligned. Compressed
/// entries, which are never mapped, are copied as they are.
pub fn align(input: &Path, output: &Path) -> Result<(), Error> {
    let mut archive = ZipArchive::new(BufReader::new(fs::File::open(input)?))?;
    let mut writer = ZipWriter::new(fs::File::create(output)?);
    for index in 0..archive.len() {
        let mut entry = archive.by_index_raw(index)?;
        if entry.compression() != CompressionMethod::Stored || entry.is_dir() {
            writer.raw_copy_file(entry)?;
            continue;
        }
        let name = entry.name()?.into_owned();
        let mut options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Stored)
            .with_alignment(alignment(&name) as u16);
        if let Some(time) = entry.last_modified() {
            options = options.last_modified_time(time);
        }
        if let Some(mode) = entry.unix_mode() {
            options = options.unix_permissions(mode);
        }
        writer.start_file(name, options)?;
        io::copy(&mut entry, &mut writer)?;
    }
    writer.finish()?;
    check(output)
}

/// Every stored entry of `path` is aligned. The ZIP writer leaves an entry unaligned
/// when the padding would not fit in its extra field, so this is checked, not assumed.
fn check(path: &Path) -> Result<(), Error> {
    let mut archive = ZipArchive::new(BufReader::new(fs::File::open(path)?))?;
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index)?;
        if entry.compression() != CompressionMethod::Stored || entry.is_dir() {
            continue;
        }
        let name = entry.name()?;
        if entry
            .data_start()
            .is_none_or(|start| start % alignment(&name) != 0)
        {
            return Err(Error::Unaligned(name.into_owned()));
        }
    }
    Ok(())
}
