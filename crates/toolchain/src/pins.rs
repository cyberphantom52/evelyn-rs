//! The files Evelyn downloads. Updating one is changing its URL and SHA-256 here.

/// A file at `path` in the data directory, downloaded from `url`: the download
/// itself, or one `member` of it, a ZIP. Each is checked by its SHA-256.
pub(crate) struct Pin {
    pub path: &'static str,
    pub url: &'static str,
    pub encoding: Encoding,
    pub sha256: &'static str,
    pub member: Option<Member>,
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

pub(crate) const PINS: [Pin; 5] = [
    Pin {
        path: "apktool.jar",
        url: "https://github.com/iBotPeaches/Apktool/releases/download/v3.0.3/apktool_3.0.3.jar",
        encoding: Encoding::Raw,
        sha256: "dbf930b076c6b9be08d57c449cacefc3bdd6b71ebd59b3066fc0e1f5b14f9423",
        member: None,
    },
    // From the Android SDK build tools, under the Android SDK license.
    Pin {
        path: "apksigner.jar",
        url: "https://dl.google.com/android/repository/build-tools_r37_linux.zip",
        encoding: Encoding::Raw,
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
        encoding: Encoding::Raw,
        sha256: "d4cedaf37d6424a6899730a2d867b108f2a47035e300df78420a29ff12e6bd7d",
        member: None,
    },
    // Android's M4 macros, which TE policies in patches are written with.
    Pin {
        path: "selinux/te_macros",
        url: sepolicy!("te_macros"),
        encoding: Encoding::Base64,
        sha256: "62c1800eba089dd920b33ec5f832d7594560f954354f9a50db01cce2a5170ae7",
        member: None,
    },
    Pin {
        path: "selinux/global_macros",
        url: sepolicy!("global_macros"),
        encoding: Encoding::Base64,
        sha256: "f79a4ef588cdadf9e5ed5d3f5565b33da56a5d2fda01edaadcfab1bac0199117",
        member: None,
    },
];
