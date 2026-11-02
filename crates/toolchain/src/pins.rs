//! The files Evelyn downloads. Updating one is changing its URL and SHA-256 here.

/// A file at `path` in the data directory, downloaded from `url`: the download
/// itself, or one `member` of it, a ZIP. Each is checked by its SHA-256.
pub(crate) struct Pin {
    pub path: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub member: Option<Member>,
}

pub(crate) struct Member {
    pub name: &'static str,
    pub sha256: &'static str,
}

impl Pin {
    /// The SHA-256 of the file as kept.
    pub fn file_sha256(&self) -> &'static str {
        self.member
            .as_ref()
            .map_or(self.sha256, |member| member.sha256)
    }
}

pub(crate) const PINS: [Pin; 3] = [
    Pin {
        path: "apktool.jar",
        url: "https://github.com/iBotPeaches/Apktool/releases/download/v3.0.3/apktool_3.0.3.jar",
        sha256: "dbf930b076c6b9be08d57c449cacefc3bdd6b71ebd59b3066fc0e1f5b14f9423",
        member: None,
    },
    // From the Android SDK build tools, under the Android SDK license.
    Pin {
        path: "apksigner.jar",
        url: "https://dl.google.com/android/repository/build-tools_r37_linux.zip",
        sha256: "01af179347cbcd9c208b7f8171f7b21f6dd1d2f85bcd15e88caa51d5d7b86060",
        member: Some(Member {
            name: "android-37.0/lib/apksigner.jar",
            sha256: "2defad215d7ff52968a409cde528cdaef7918b115e276b8e3378ca7a178e4180",
        }),
    },
    // VNDK APEXes for vendors older than Android 15, which ROMs from Android 15 on
    // no longer ship; built from AOSP and published with Evelyn.
    Pin {
        path: "apex/com.android.vndk.v32.apex",
        url: "https://github.com/cyberphantom52/evelyn-rs/releases/download/vndk/com.android.vndk.v32.apex",
        sha256: "d4cedaf37d6424a6899730a2d867b108f2a47035e300df78420a29ff12e6bd7d",
        member: None,
    },
];
