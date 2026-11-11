//! Where a device's split SELinux policy is.

/// Where the vendor names the platform policy version its own policy is written against.
pub const SEPOLICY_VERSION: &str = "/vendor/etc/selinux/plat_sepolicy_vers.txt";

/// The CIL files init compiles a device's split SELinux policy from, in the order it
/// passes them to secilc. A file with a suffix is named for the platform policy
/// version the vendor targets, between the two: `/system/etc/selinux/mapping/` +
/// version + `.cil`. Init requires the platform policy and its mapping,
/// `plat_pub_versioned.cil` and the vendor's policy, and uses each of the others only
/// if the device has it.
pub const SEPOLICY_FILES: [(&str, Option<&str>); 11] = [
    ("/system/etc/selinux/plat_sepolicy.cil", None),
    ("/system/etc/selinux/mapping/", Some(".cil")),
    ("/system/etc/selinux/mapping/", Some(".compat.cil")),
    ("/system_ext/etc/selinux/system_ext_sepolicy.cil", None),
    ("/system_ext/etc/selinux/mapping/", Some(".cil")),
    ("/system_ext/etc/selinux/mapping/", Some(".compat.cil")),
    ("/product/etc/selinux/product_sepolicy.cil", None),
    ("/product/etc/selinux/mapping/", Some(".cil")),
    ("/vendor/etc/selinux/plat_pub_versioned.cil", None),
    ("/vendor/etc/selinux/vendor_sepolicy.cil", None),
    ("/odm/etc/selinux/odm_sepolicy.cil", None),
];
