//! The stock firmware's vbmeta images, re-signed for what the build wrote.

use crate::engine::firmware::OpenFirmware;
use crate::engine::{Error, IoAt};
use bootimg::avb::{self, Descriptor, Locator, Signer, VbMeta};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
    path::PathBuf,
};

/// The locator at the end of a partition image with an AVB footer.
const LOCATOR: usize = 64;

/// Re-sign into `images/` in the work directory every vbmeta image of `stock`,
/// with the descriptor of each partition image: that of an image the build
/// wrote from `built`, and that of an image it copies from the image's own AVB
/// footer, as the firmware may hold images other than those its vbmeta was
/// signed for. A chain to a boot image the build rebuilt takes its key in
/// `keys`. Each is by name.
pub(crate) fn rebuild(
    work_dir: &Path,
    stock: &OpenFirmware,
    built: &BTreeMap<String, Descriptor>,
    keys: BTreeMap<String, Vec<u8>>,
) -> Result<Vec<(String, PathBuf)>, Error> {
    let mut vbmetas = Vec::new();
    let mut descriptors = BTreeMap::new();
    for path in stock.firmware_images()? {
        let mut file = fs::File::open(&path).at(&path)?;
        let mut magic = [0; 4];
        file.read_exact(&mut magic).at(&path)?;
        if &magic == b"AVB0" {
            let name = path.file_stem().unwrap_or_default().to_string_lossy();
            let data = fs::read(&path).at(&path)?;
            let vbmeta = VbMeta::parse(&data).map_err(android::Error::from)?;
            vbmetas.push((name.into_owned(), vbmeta));
        } else if let Some(vbmeta) = footer(&mut file).at(&path)? {
            let vbmeta = VbMeta::parse(&vbmeta).map_err(android::Error::from)?;
            for descriptor in vbmeta.descriptors {
                if let Descriptor::Hash { partition_name, .. }
                | Descriptor::Hashtree { partition_name, .. } = &descriptor
                {
                    descriptors.insert(partition_name.clone(), descriptor);
                }
            }
        }
    }
    descriptors.extend(built.iter().map(|(name, d)| (name.clone(), d.clone())));
    let signed = avb::resign(vbmetas, &descriptors, keys, &Signer::TestKeys)
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

/// The vbmeta of `image`'s AVB footer, if it has one, read from where its
/// locator says.
fn footer(image: &mut fs::File) -> io::Result<Option<Vec<u8>>> {
    let mut locator = [0; LOCATOR];
    image.seek(SeekFrom::End(-(LOCATOR as i64)))?;
    image.read_exact(&mut locator)?;
    let Some(locator) = Locator::parse(&locator) else {
        return Ok(None);
    };
    let mut vbmeta = vec![0; locator.vbmeta_size as usize];
    image.seek(SeekFrom::Start(locator.vbmeta_offset))?;
    image.read_exact(&mut vbmeta)?;
    Ok(Some(vbmeta))
}
