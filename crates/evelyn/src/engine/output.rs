//! Publish built images in the requested output format.

use crate::engine::build::Written;
use crate::engine::firmware::OpenFirmware;
use crate::engine::{Error, IoAt, Output, Profile};
use android::hash::Sha256Digest;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Move what was built into `output_dir` as `output`, with the boot images
/// whose ramdisk the build changed. A flashable ZIP also flashes the stock
/// firmware's partitions outside `super`, those boot images in place of their
/// stock ones, and is named for its SHA-256.
pub(crate) fn publish(
    output_dir: &Path,
    profile: &Profile,
    output: Output,
    stock: &OpenFirmware,
    images: Written,
    boot: Vec<(String, PathBuf)>,
) -> Result<Vec<PathBuf>, Error> {
    fs::create_dir_all(output_dir).at(output_dir)?;
    Ok(match images {
        Written::Partitions(images) => images_into(output_dir, images.into_iter().chain(boot))?,
        Written::Super(super_image) if output == Output::Super => {
            let output = output_dir.join("super.img");
            fs::rename(&super_image, &output).at(&super_image)?;
            let mut outputs = vec![output];
            outputs.extend(images_into(output_dir, boot)?);
            outputs
        }
        Written::Super(super_image) => {
            let mut firmware: Vec<PathBuf> = stock
                .firmware_images()?
                .into_iter()
                .filter(|image| {
                    let name = image.file_stem().unwrap_or_default();
                    !boot.iter().any(|(boot, _)| name == boot.as_str())
                })
                .collect();
            firmware.extend(boot.iter().map(|(_, image)| image.clone()));
            let partial = output_dir.join("evelyn.zip.partial");
            crate::engine::flashable::write(
                &crate::engine::flashable::Contents {
                    device: &profile.device.product_id,
                    super_image: &super_image,
                    firmware: &firmware,
                },
                &partial,
            )?;
            fs::remove_file(&super_image).at(&super_image)?;
            for (_, image) in &boot {
                fs::remove_file(image).at(image)?;
            }
            // The ZIP's writer seeks back to finish headers, so it is hashed once written.
            let sha256 =
                Sha256Digest::reader(fs::File::open(&partial).at(&partial)?).at(&partial)?;
            let output = output_dir.join(format!("{sha256}.zip"));
            fs::rename(&partial, &output).at(&partial)?;
            vec![output]
        }
    })
}

/// Move each `<name>` image into `output_dir` as `<name>.img`.
fn images_into(
    output_dir: &Path,
    images: impl IntoIterator<Item = (String, PathBuf)>,
) -> Result<Vec<PathBuf>, Error> {
    images
        .into_iter()
        .map(|(name, image)| {
            let output = output_dir.join(format!("{name}.img"));
            fs::rename(&image, &output).at(&image)?;
            Ok(output)
        })
        .collect()
}
