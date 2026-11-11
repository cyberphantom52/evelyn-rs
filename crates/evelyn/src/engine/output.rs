//! Publish built images in the requested output format.

use crate::engine::build::Written;
use crate::engine::firmware::Firmware;
use crate::engine::{Error, Output, Profile};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Move what was built into `output_dir` as `output`. A flashable ZIP also
/// flashes the stock firmware's partitions outside `super`.
pub(crate) fn publish(
    output_dir: &Path,
    profile: &Profile,
    output: Output,
    stock: &Firmware,
    images: Written,
) -> Result<Vec<PathBuf>, Error> {
    fs::create_dir_all(output_dir)?;
    Ok(match images {
        Written::Partitions(images) => images
            .into_iter()
            .map(|(name, image)| {
                let output = output_dir.join(format!("{name}.img"));
                fs::rename(image, &output)?;
                Ok(output)
            })
            .collect::<Result<_, Error>>()?,
        Written::Super(super_image) if output == Output::Super => {
            let output = output_dir.join("super.img");
            fs::rename(super_image, &output)?;
            vec![output]
        }
        Written::Super(super_image) => {
            let output = output_dir.join(format!("{}.zip", profile.device.name));
            crate::engine::flashable::write(
                &crate::engine::flashable::Package {
                    device: &profile.device.product_ids[0],
                    super_image: &super_image,
                    firmware: &stock.firmware_images()?,
                },
                &output,
            )?;
            fs::remove_file(super_image)?;
            vec![output]
        }
    })
}
