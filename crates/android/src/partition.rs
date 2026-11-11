//! Partitions: which side of Treble each is on, where each is mounted, and the
//! partitions and paths a device path refers to.

use crate::path::{DevicePath, RelativePath, glob};

/// A path in a partition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartitionPath {
    pub partition: String,
    pub path: RelativePath,
}

/// Where each partition is mounted on the device, for resolving device paths.
#[derive(Clone, Debug, Default)]
pub struct Mounts(Vec<(String, String)>);

impl Mounts {
    /// Mount points are absolute, like `/` or `/vendor`.
    pub fn new(mounts: impl IntoIterator<Item = (String, String)>) -> Self {
        Self(mounts.into_iter().collect())
    }

    /// The partitions and partition-relative paths `path` refers to. A literal path
    /// belongs to the partition with the longest matching mount point; where globs
    /// in its leading components match other mount points, those partitions too.
    pub fn resolve(&self, path: &DevicePath) -> Vec<PartitionPath> {
        let components: Vec<&str> = path.components().collect();
        let mut literal: Option<(usize, PartitionPath)> = None;
        let mut globbed = Vec::new();
        for (mount, partition) in &self.0 {
            let mount: Vec<&str> = mount.split('/').filter(|c| !c.is_empty()).collect();
            if mount.len() >= components.len()
                || !mount.iter().zip(&components).all(|(m, c)| glob(c, m))
            {
                continue;
            }
            let Ok(rest) = RelativePath::try_from(components[mount.len()..].join("/")) else {
                continue;
            };
            let target = PartitionPath {
                partition: partition.clone(),
                path: rest,
            };
            if components[..mount.len()]
                .iter()
                .any(|c| c.contains(['*', '?']))
            {
                globbed.push(target);
            } else if literal
                .as_ref()
                .is_none_or(|(depth, _)| mount.len() > *depth)
            {
                literal = Some((mount.len(), target));
            }
        }
        literal
            .map(|(_, target)| target)
            .into_iter()
            .chain(globbed)
            .collect()
    }

    /// The directory a partition provides on the device: its mount point, or for
    /// the partition mounted at `/` (a system-as-root `system`), `/<name>`.
    pub fn directory(&self, partition: &str) -> Option<String> {
        let (mount, _) = self.0.iter().find(|(_, p)| p == partition)?;
        Some(if mount == "/" {
            format!("/{partition}")
        } else {
            mount.clone()
        })
    }

    /// The partition mounted at exactly `path`, if one is.
    pub fn partition_at(&self, path: &DevicePath) -> Option<&String> {
        let path = path.to_string();
        self.0
            .iter()
            .find(|(mount, _)| mount.trim_end_matches('/') == path.trim_end_matches('/'))
            .map(|(_, partition)| partition)
    }

    /// The device path of `path` in `partition`, if the partition is mounted.
    pub fn device_path(&self, target: &PartitionPath) -> Option<DevicePath> {
        let (mount, _) = self.0.iter().find(|(_, p)| *p == target.partition)?;
        DevicePath::try_from(format!("{}/{}", mount.trim_end_matches('/'), target.path)).ok()
    }
}

/// Treble's vendor side, in the order partitions are laid out in `super`. A device
/// lists the ones it has; recipes may name any of them, for conditions.
pub const VENDOR_SIDE: [&str; 4] = ["odm", "vendor", "vendor_dlkm", "odm_dlkm"];
/// System-side partitions every port takes from the ROM, in `super` order.
pub const STANDARD_SYSTEM: [&str; 4] = ["system", "system_ext", "product", "system_dlkm"];

/// The mount point a partition gets unless the port overrides it: `/<name>`, except
/// `system`, which depends on the image (see `system_mount_point`).
pub fn default_mount_point(partition: &str) -> Option<String> {
    (partition != "system").then(|| format!("/{partition}"))
}

/// `/` for a system-as-root `system` image, whose top level is the device's root
/// with `/system` beneath it, or `/system` for an older one, whose top level is
/// `/system` itself.
pub fn system_mount_point(has: impl Fn(&str) -> bool) -> Option<&'static str> {
    if has("system/build.prop") {
        Some("/")
    } else if has("build.prop") {
        Some("/system")
    } else {
        None
    }
}
