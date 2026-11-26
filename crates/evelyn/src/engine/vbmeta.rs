//! The stock firmware's vbmeta images, re-signed for what the build wrote.

use crate::engine::firmware::OpenFirmware;
use crate::engine::{Error, IoAt};
use bootimg::avb::{self, Descriptor, Signer, VbMeta};
use std::{collections::BTreeMap, fs, path::Path, path::PathBuf};

/// Re-sign into `images/` in the work directory every vbmeta image of `stock`,
/// with the `descriptors` of the partitions the build wrote and the `keys` the
/// boot images it rebuilt are signed with: each by name.
pub(crate) fn rebuild(
    work_dir: &Path,
    stock: &OpenFirmware,
    descriptors: &BTreeMap<String, Descriptor>,
    keys: BTreeMap<String, Vec<u8>>,
) -> Result<Vec<(String, PathBuf)>, Error> {
    let mut vbmetas = Vec::new();
    for path in stock.firmware_images()? {
        let data = fs::read(&path).at(&path)?;
        if data.starts_with(b"AVB0") {
            let name = path.file_stem().unwrap_or_default().to_string_lossy();
            let vbmeta = VbMeta::parse(&data).map_err(android::Error::from)?;
            vbmetas.push((name.into_owned(), vbmeta));
        }
    }
    let signed = avb::resign(vbmetas, descriptors, keys, &Signer::TestKeys)
        .map_err(android::Error::from)?;
    let dir = work_dir.join("images");
    signed
        .into_iter()
        .map(|(name, blob)| {
            let image = dir.join(format!("{name}.img"));
            fs::write(&image, blob).at(&image)?;
            Ok((name, image))
        })
        .collect()
}
