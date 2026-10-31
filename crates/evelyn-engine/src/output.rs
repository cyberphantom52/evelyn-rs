//! Publish built images in the requested output format.

use crate::build::Written;
use crate::{BuildRequest, Error};
use std::{fs, path::PathBuf};

pub(crate) fn publish(request: &BuildRequest<'_>, images: Written) -> Result<Vec<PathBuf>, Error> {
    fs::create_dir_all(request.output_dir)?;
    let profile = &request.plan.profile;
    Ok(match images {
        Written::Partitions(images) => images
            .into_iter()
            .map(|(name, image)| {
                let output = request.output_dir.join(format!("{name}.img"));
                fs::rename(image, &output)?;
                Ok(output)
            })
            .collect::<Result<_, Error>>()?,
        Written::Super(super_image) if request.plan.output == crate::Output::Super => {
            let output = request.output_dir.join("super.img");
            fs::rename(super_image, &output)?;
            vec![output]
        }
        Written::Super(super_image) => {
            let output = request
                .output_dir
                .join(format!("{}.zip", profile.device.device));
            crate::flashable::write(
                &crate::flashable::Package {
                    template: &request.tools.file("packaging/template")?,
                    device: &profile.device.product_ids[0],
                    super_image: &super_image,
                    firmware: &request.plan.firmware,
                },
                &output,
            )?;
            fs::remove_file(super_image)?;
            vec![output]
        }
    })
}
