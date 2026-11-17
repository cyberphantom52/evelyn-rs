//! Publish built images in the requested output format.

use crate::engine::build::Written;
use crate::engine::firmware::OpenFirmware;
use crate::engine::{Error, IoAt, Output, Profile};
use android::hash::Sha256Digest;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Move what was built into `output_dir` as `output`. A flashable ZIP also
/// flashes the stock firmware's partitions outside `super`, and is named for its
/// SHA-256.
pub(crate) fn publish(
    output_dir: &Path,
    profile: &Profile,
    output: Output,
    stock: &OpenFirmware,
    images: Written,
) -> Result<Vec<PathBuf>, Error> {
    fs::create_dir_all(output_dir).at(output_dir)?;
    Ok(match images {
        Written::Partitions(images) => images
            .into_iter()
            .map(|(name, image)| {
                let output = output_dir.join(format!("{name}.img"));
                fs::rename(&image, &output).at(&image)?;
                Ok(output)
            })
            .collect::<Result<_, Error>>()?,
        Written::Super(super_image) if output == Output::Super => {
            let output = output_dir.join("super.img");
            fs::rename(&super_image, &output).at(&super_image)?;
            vec![output]
        }
        Written::Super(super_image) => {
            let partial = output_dir.join("evelyn.zip.partial");
            crate::engine::flashable::write(
                &crate::engine::flashable::Contents {
                    device: &profile.device.product_id,
                    super_image: &super_image,
                    firmware: &stock.firmware_images()?,
                },
                &partial,
            )?;
            fs::remove_file(&super_image).at(&super_image)?;
            // The ZIP's writer seeks back to finish headers, so it is hashed once written.
            let sha256 =
                Sha256Digest::reader(fs::File::open(&partial).at(&partial)?).at(&partial)?;
            let output = output_dir.join(format!("{sha256}.zip"));
            fs::rename(&partial, &output).at(&partial)?;
            vec![output]
        }
    })
}
