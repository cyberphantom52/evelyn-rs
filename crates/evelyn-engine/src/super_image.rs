//! Super (dynamic partitions) images, built with `lpmake`.

use crate::Error;
use android_firmware::Name;
use recipe::{SlotScheme, SuperLayout};
use std::{fs, path::Path, path::PathBuf};
use toolchain::Tools;

/// Build a raw super image holding `images`, in order, in the layout's one group.
/// A/B layouts fill slot `a` and leave empty `b` partitions.
pub(crate) fn build(
    tools: &Tools,
    layout: &SuperLayout,
    images: &[(Name, PathBuf)],
    output: &Path,
) -> Result<(), Error> {
    let mut command = tools.command("lpmake")?;
    command
        .arg(format!("--device-size={}", layout.size))
        .arg(format!("--metadata-size={}", layout.metadata_size))
        .arg(format!("--metadata-slots={}", layout.metadata_slots()))
        .args(["--super-name=super", "--force-full-image"]);
    if matches!(layout.slots, SlotScheme::VirtualAb) {
        command.arg("--virtual-ab");
    }
    let suffixes: &[&str] = match layout.slots {
        SlotScheme::Single => &[""],
        SlotScheme::Ab | SlotScheme::VirtualAb => &["_a", "_b"],
    };
    for (index, suffix) in suffixes.iter().enumerate() {
        let group = format!("{}{suffix}", layout.group);
        command.arg(format!("--group={group}:{}", layout.group_capacity()));
        for (partition, image) in images {
            let name = format!("{partition}{suffix}");
            let size = if index == 0 {
                command.arg(format!("--image={name}={}", image.display()));
                fs::metadata(image)?.len()
            } else {
                0
            };
            command.arg(format!("--partition={name}:readonly:{size}:{group}"));
        }
    }
    toolchain::run(command.arg(format!("--output={}", output.display())))?;
    Ok(())
}
