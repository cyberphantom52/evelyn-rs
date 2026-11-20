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

/// The VNDK APEX of `version`, published with Evelyn in the `vndk` release.
macro_rules! vndk {
    ($version:literal, $sha256:literal) => {
        Pin {
            path: concat!("apex/com.android.vndk.v", $version, ".apex"),
            url: concat!(
                "https://github.com/cyberphantom52/evelyn-rs/releases/download/vndk/",
                "com.android.vndk.v",
                $version,
                ".apex"
            ),
            encoding: Encoding::Raw,
            sha256: $sha256,
        }
    };
}

pub(crate) const PINS: [Pin; 7] = [
    // VNDK APEXes for vendors older than Android 15, which ROMs from Android 15 on
    // no longer ship; published with Evelyn.
    vndk!("30", "52b2ea015b7df7fa0ae4a1027a9b3b48f68dc246daf86a91f553ec01ada9223c"),
    vndk!("31", "1eea0e1f314624137082dbf1321efdfb7c82a836802d58875fe8beb12e9a4c4f"),
    vndk!("32", "d4cedaf37d6424a6899730a2d867b108f2a47035e300df78420a29ff12e6bd7d"),
    vndk!("33", "1911ff1bea746bf33e4b358469a313db68d3e437a3bcf9bd188c5fa144428797"),
    vndk!("34", "344a628641751a43e15032837b6989ea220bd2b667d10e974c9548c474bdfc1c"),
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
