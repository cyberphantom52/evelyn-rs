//! Publish built images in the requested output format.

use crate::{BuildRequest, Error};
use android_firmware::Name;
use model::Output;
use std::{fs, path::PathBuf};

/// `images` are in `super` order.
pub(crate) fn publish(
    request: &BuildRequest<'_>,
    images: &[(Name, PathBuf)],
) -> Result<Vec<PathBuf>, Error> {
    fs::create_dir_all(request.output_dir)?;
    let profile = &request.plan.profile;
    let outputs = match request.plan.output {
        Output::Partitions => images
            .iter()
            .map(|(name, image)| {
                let output = request.output_dir.join(format!("{name}.img"));
                fs::rename(image, &output)?;
                Ok(output)
            })
            .collect::<Result<_, Error>>()?,
        Output::Super => {
            let layout = profile
                .device
                .super_layout
                .as_ref()
                .expect("checked by Plan::load");
            let output = request.output_dir.join("super.img");
            tracing::info!(path = %output.display(), "building super image");
            crate::super_image::build(request.tools, layout, images, &output)?;
            vec![output]
        }
        Output::Zip => {
            let layout = profile
                .device
                .super_layout
                .as_ref()
                .expect("checked by Plan::load");
            let super_image = request.work_dir.join("images").join("super.img");
            tracing::info!("building super image");
            crate::super_image::build(request.tools, layout, images, &super_image)?;
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
    };
    Ok(outputs)
}
