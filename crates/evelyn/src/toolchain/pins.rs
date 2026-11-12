//! The files Evelyn downloads. Updating one is changing its URL and SHA-256 here.

/// A file at `path` in the data directory, downloaded from `url` and checked by
/// its SHA-256.
pub(crate) struct Pin {
    pub path: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
}

pub(crate) const PINS: [Pin; 2] = [
    Pin {
        path: "apktool.jar",
        url: "https://github.com/iBotPeaches/Apktool/releases/download/v3.0.3/apktool_3.0.3.jar",
        sha256: "dbf930b076c6b9be08d57c449cacefc3bdd6b71ebd59b3066fc0e1f5b14f9423",
    },
    // VNDK APEXes for vendors older than Android 15, which ROMs from Android 15 on
    // no longer ship; built from AOSP and published with Evelyn.
    Pin {
        path: "apex/com.android.vndk.v32.apex",
        url: "https://github.com/cyberphantom52/evelyn-rs/releases/download/vndk/com.android.vndk.v32.apex",
        sha256: "d4cedaf37d6424a6899730a2d867b108f2a47035e300df78420a29ff12e6bd7d",
    },
];
