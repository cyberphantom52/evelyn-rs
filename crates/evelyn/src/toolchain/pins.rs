//! The files Evelyn downloads. Updating one is changing its URL and SHA-256 here.

/// A file at `path` in the data directory, downloaded from `url` and checked by
/// its SHA-256.
pub(crate) struct Pin {
    pub path: &'static str,
    pub url: &'static str,
    pub encoding: Encoding,
    pub sha256: &'static str,
}

/// How `url` serves the file. The SHA-256 is of the file once decoded.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Encoding {
    /// As it is.
    Raw,
    /// In base64, as Gitiles serves a file with `?format=TEXT`.
    Base64,
}

/// The AOSP commit of `system/sepolicy` that Android's M4 macros are taken from.
macro_rules! sepolicy {
    ($file:literal) => {
        concat!(
            "https://android.googlesource.com/platform/system/sepolicy/+/",
            "4571ddd9440721fec583c906a337de949a77749e/public/",
            $file,
            "?format=TEXT"
        )
    };
}

pub(crate) const PINS: [Pin; 3] = [
    // VNDK APEXes for vendors older than Android 15, which ROMs from Android 15 on
    // no longer ship; built from AOSP and published with Evelyn.
    Pin {
        path: "apex/com.android.vndk.v32.apex",
        url: "https://github.com/cyberphantom52/evelyn-rs/releases/download/vndk/com.android.vndk.v32.apex",
        encoding: Encoding::Raw,
        sha256: "d4cedaf37d6424a6899730a2d867b108f2a47035e300df78420a29ff12e6bd7d",
    },
    // Android's M4 macros, which TE policies in patches are written with.
    Pin {
        path: "selinux/te_macros",
        url: sepolicy!("te_macros"),
        encoding: Encoding::Base64,
        sha256: "62c1800eba089dd920b33ec5f832d7594560f954354f9a50db01cce2a5170ae7",
    },
    Pin {
        path: "selinux/global_macros",
        url: sepolicy!("global_macros"),
        encoding: Encoding::Base64,
        sha256: "f79a4ef588cdadf9e5ed5d3f5565b33da56a5d2fda01edaadcfab1bac0199117",
    },
];
