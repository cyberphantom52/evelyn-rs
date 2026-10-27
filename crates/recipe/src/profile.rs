//! Treble partition names and Android system mount layouts.

/// Treble's vendor side, in the order partitions are laid out in `super`. A device
/// lists the ones it has; recipes may name any of them, for conditions.
pub const VENDOR_SIDE: [&str; 4] = ["odm", "vendor", "vendor_dlkm", "odm_dlkm"];
/// System-side partitions every port takes from the ROM, in `super` order.
pub const STANDARD_SYSTEM: [&str; 4] = ["system", "system_ext", "product", "system_dlkm"];
/// The firmware a partition or import comes from.
pub const ROM: &str = "rom";
pub const STOCK: &str = "stock";

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
