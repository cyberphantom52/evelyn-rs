//! Android's `fs_config`, as libcutils' `fs_config.cpp` implements it: the
//! partitions' `etc/fs_config_files` and `etc/fs_config_dirs` first, in the order
//! Android reads them, then the built-in table; the first match wins, and a path
//! nothing matches is `root:root`, `0755` for a directory, `0644` for a file.
//! Paths are as on the device, without the leading `/`: `vendor/bin/foo`.

use super::Error;

const AID_ROOT: u32 = 0;
const AID_SYSTEM: u32 = 1000;
const AID_DHCP: u32 = 1014;
const AID_MEDIA_RW: u32 = 1023;
const AID_SDCARD_R: u32 = 1028;
const AID_LOGD: u32 = 1036;
const AID_SHARED_RELRO: u32 = 1037;
const AID_SHELL: u32 = 2000;
const AID_CACHE: u32 = 2001;
const AID_MISC: u32 = 9998;
const AID_APP: u32 = 10000;

const CAP_SETGID: u64 = 1 << 6;
const CAP_SETUID: u64 = 1 << 7;
const CAP_BLOCK_SUSPEND: u64 = 1 << 36;

/// What `fs_config` gives a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ownership {
    pub uid: u32,
    pub gid: u32,
    /// Permission bits, setuid and setgid included.
    pub mode: u16,
    pub capabilities: u64,
}

struct Rule {
    prefix: String,
    ownership: Ownership,
}

const fn rule(
    mode: u16,
    uid: u32,
    gid: u32,
    capabilities: u64,
    prefix: &'static str,
) -> (Ownership, &'static str) {
    (
        Ownership {
            uid,
            gid,
            mode,
            capabilities,
        },
        prefix,
    )
}

/// libcutils' `android_dirs`.
const DIRS: &[(Ownership, &str)] = &[
    rule(0o770, AID_SYSTEM, AID_CACHE, 0, "cache"),
    rule(0o555, AID_ROOT, AID_ROOT, 0, "config"),
    rule(0o771, AID_SYSTEM, AID_SYSTEM, 0, "data/app"),
    rule(0o771, AID_SYSTEM, AID_SYSTEM, 0, "data/app-private"),
    rule(0o771, AID_SYSTEM, AID_SYSTEM, 0, "data/app-ephemeral"),
    rule(0o771, AID_ROOT, AID_ROOT, 0, "data/dalvik-cache"),
    rule(0o771, AID_SYSTEM, AID_SYSTEM, 0, "data/data"),
    rule(0o771, AID_SHELL, AID_SHELL, 0, "data/local/tmp"),
    rule(0o771, AID_SHELL, AID_SHELL, 0, "data/local"),
    rule(0o770, AID_DHCP, AID_DHCP, 0, "data/misc/dhcp"),
    rule(
        0o771,
        AID_SHARED_RELRO,
        AID_SHARED_RELRO,
        0,
        "data/misc/shared_relro",
    ),
    rule(0o1771, AID_SYSTEM, AID_MISC, 0, "data/misc"),
    rule(0o775, AID_MEDIA_RW, AID_MEDIA_RW, 0, "data/media/Music"),
    rule(0o775, AID_MEDIA_RW, AID_MEDIA_RW, 0, "data/media"),
    rule(0o775, AID_ROOT, AID_ROOT, 0, "data/preloads"),
    rule(0o771, AID_SYSTEM, AID_SYSTEM, 0, "data"),
    rule(0o755, AID_ROOT, AID_SYSTEM, 0, "mnt"),
    rule(0o751, AID_ROOT, AID_SHELL, 0, "product/bin"),
    rule(0o777, AID_ROOT, AID_ROOT, 0, "sdcard"),
    rule(0o751, AID_ROOT, AID_SDCARD_R, 0, "storage"),
    rule(0o751, AID_ROOT, AID_SHELL, 0, "system/bin"),
    rule(0o755, AID_ROOT, AID_ROOT, 0, "system/etc/ppp"),
    rule(0o755, AID_ROOT, AID_SHELL, 0, "system/vendor"),
    rule(0o750, AID_ROOT, AID_SHELL, 0, "system/xbin"),
    rule(0o751, AID_ROOT, AID_SHELL, 0, "system_ext/bin"),
    rule(0o751, AID_ROOT, AID_SHELL, 0, "vendor/bin"),
    rule(0o755, AID_ROOT, AID_SHELL, 0, "vendor"),
];

/// libcutils' `android_files`.
const FILES: &[(Ownership, &str)] = &[
    rule(0o644, AID_SYSTEM, AID_SYSTEM, 0, "data/app/*"),
    rule(0o644, AID_SYSTEM, AID_SYSTEM, 0, "data/app-ephemeral/*"),
    rule(0o644, AID_SYSTEM, AID_SYSTEM, 0, "data/app-private/*"),
    rule(0o644, AID_APP, AID_APP, 0, "data/data/*"),
    rule(0o644, AID_MEDIA_RW, AID_MEDIA_RW, 0, "data/media/*"),
    rule(0o600, AID_ROOT, AID_ROOT, 0, "default.prop"),
    rule(0o600, AID_ROOT, AID_ROOT, 0, "system/etc/prop.default"),
    rule(0o600, AID_ROOT, AID_ROOT, 0, "odm/build.prop"),
    rule(0o600, AID_ROOT, AID_ROOT, 0, "odm/default.prop"),
    rule(0o600, AID_ROOT, AID_ROOT, 0, "odm/etc/build.prop"),
    rule(0o444, AID_ROOT, AID_ROOT, 0, "odm/etc/fs_config_dirs"),
    rule(0o444, AID_ROOT, AID_ROOT, 0, "odm/etc/fs_config_files"),
    rule(0o444, AID_ROOT, AID_ROOT, 0, "oem/etc/fs_config_dirs"),
    rule(0o444, AID_ROOT, AID_ROOT, 0, "oem/etc/fs_config_files"),
    rule(0o600, AID_ROOT, AID_ROOT, 0, "product/build.prop"),
    rule(0o444, AID_ROOT, AID_ROOT, 0, "product/etc/fs_config_dirs"),
    rule(0o444, AID_ROOT, AID_ROOT, 0, "product/etc/fs_config_files"),
    rule(0o600, AID_ROOT, AID_ROOT, 0, "system_ext/build.prop"),
    rule(
        0o444,
        AID_ROOT,
        AID_ROOT,
        0,
        "system_ext/etc/fs_config_dirs",
    ),
    rule(
        0o444,
        AID_ROOT,
        AID_ROOT,
        0,
        "system_ext/etc/fs_config_files",
    ),
    rule(0o755, AID_ROOT, AID_SHELL, 0, "system/bin/crash_dump32"),
    rule(0o755, AID_ROOT, AID_SHELL, 0, "system/bin/crash_dump64"),
    rule(0o755, AID_ROOT, AID_SHELL, 0, "system/bin/debuggerd"),
    rule(0o550, AID_LOGD, AID_LOGD, 0, "system/bin/logd"),
    rule(0o700, AID_ROOT, AID_ROOT, 0, "system/bin/secilc"),
    rule(0o750, AID_ROOT, AID_ROOT, 0, "system/bin/uncrypt"),
    rule(0o600, AID_ROOT, AID_ROOT, 0, "system/build.prop"),
    rule(0o444, AID_ROOT, AID_ROOT, 0, "system/etc/fs_config_dirs"),
    rule(0o444, AID_ROOT, AID_ROOT, 0, "system/etc/fs_config_files"),
    rule(0o440, AID_ROOT, AID_SHELL, 0, "system/etc/init.goldfish.rc"),
    rule(0o550, AID_ROOT, AID_SHELL, 0, "system/etc/init.goldfish.sh"),
    rule(0o550, AID_ROOT, AID_SHELL, 0, "system/etc/init.ril"),
    rule(0o555, AID_ROOT, AID_ROOT, 0, "system/etc/ppp/*"),
    rule(0o555, AID_ROOT, AID_ROOT, 0, "system/etc/rc.*"),
    rule(
        0o750,
        AID_ROOT,
        AID_ROOT,
        0,
        "vendor/bin/install-recovery.sh",
    ),
    rule(0o600, AID_ROOT, AID_ROOT, 0, "vendor/build.prop"),
    rule(0o600, AID_ROOT, AID_ROOT, 0, "vendor/default.prop"),
    rule(0o440, AID_ROOT, AID_ROOT, 0, "vendor/etc/recovery.img"),
    rule(0o444, AID_ROOT, AID_ROOT, 0, "vendor/etc/fs_config_dirs"),
    rule(0o444, AID_ROOT, AID_ROOT, 0, "vendor/etc/fs_config_files"),
    rule(0o6755, AID_ROOT, AID_ROOT, 0, "system/xbin/procmem"),
    rule(0o4750, AID_ROOT, AID_SHELL, 0, "system/xbin/su"),
    rule(
        0o700,
        AID_SYSTEM,
        AID_SHELL,
        CAP_BLOCK_SUSPEND,
        "system/bin/inputflinger",
    ),
    rule(
        0o750,
        AID_ROOT,
        AID_SHELL,
        CAP_SETUID | CAP_SETGID,
        "system/bin/run-as",
    ),
    rule(
        0o750,
        AID_ROOT,
        AID_SHELL,
        CAP_SETUID | CAP_SETGID,
        "system/bin/simpleperf_app_runner",
    ),
    rule(
        0o755,
        AID_ROOT,
        AID_ROOT,
        0,
        "first_stage_ramdisk/system/bin/e2fsck",
    ),
    rule(
        0o755,
        AID_ROOT,
        AID_ROOT,
        0,
        "first_stage_ramdisk/system/bin/linker64",
    ),
    rule(
        0o755,
        AID_ROOT,
        AID_ROOT,
        0,
        "first_stage_ramdisk/system/bin/resize2fs",
    ),
    rule(
        0o755,
        AID_ROOT,
        AID_ROOT,
        0,
        "first_stage_ramdisk/system/bin/snapuserd",
    ),
    rule(
        0o755,
        AID_ROOT,
        AID_ROOT,
        0,
        "first_stage_ramdisk/system/bin/snapuserd_ramdisk",
    ),
    rule(
        0o755,
        AID_ROOT,
        AID_ROOT,
        0,
        "first_stage_ramdisk/system/bin/tune2fs",
    ),
    rule(
        0o755,
        AID_ROOT,
        AID_ROOT,
        0,
        "first_stage_ramdisk/system/bin/fsck.f2fs",
    ),
    rule(0o755, AID_ROOT, AID_ROOT, 0, "bin/*"),
    rule(0o755, AID_ROOT, AID_ROOT, 0, "first_stage.sh"),
    rule(0o640, AID_ROOT, AID_SHELL, 0, "fstab.*"),
    rule(0o750, AID_ROOT, AID_SHELL, 0, "init*"),
    rule(0o644, AID_ROOT, AID_ROOT, 0, "*.rc"),
    rule(0o755, AID_ROOT, AID_SHELL, 0, "odm/bin/*"),
    rule(0o644, AID_ROOT, AID_ROOT, 0, "odm/framework/*"),
    rule(0o644, AID_ROOT, AID_ROOT, 0, "odm/app/*"),
    rule(0o644, AID_ROOT, AID_ROOT, 0, "odm/priv-app/*"),
    rule(0o755, AID_ROOT, AID_SHELL, 0, "product/bin/*"),
    rule(0o644, AID_ROOT, AID_ROOT, 0, "product/framework/*"),
    rule(0o644, AID_ROOT, AID_ROOT, 0, "product/app/*"),
    rule(0o644, AID_ROOT, AID_ROOT, 0, "product/priv-app/*"),
    rule(0o755, AID_ROOT, AID_SHELL, 0, "system/bin/*"),
    rule(0o755, AID_ROOT, AID_SHELL, 0, "system/xbin/*"),
    rule(0o644, AID_ROOT, AID_ROOT, 0, "system/framework/*"),
    rule(0o644, AID_ROOT, AID_ROOT, 0, "system/app/*"),
    rule(0o644, AID_ROOT, AID_ROOT, 0, "system/priv-app/*"),
    rule(0o755, AID_ROOT, AID_SHELL, 0, "system_ext/bin/*"),
    rule(0o644, AID_ROOT, AID_ROOT, 0, "system_ext/framework/*"),
    rule(0o644, AID_ROOT, AID_ROOT, 0, "system_ext/app/*"),
    rule(0o644, AID_ROOT, AID_ROOT, 0, "system_ext/priv-app/*"),
    rule(0o755, AID_ROOT, AID_SHELL, 0, "vendor/bin/*"),
    rule(0o755, AID_ROOT, AID_SHELL, 0, "vendor/xbin/*"),
    rule(0o644, AID_ROOT, AID_ROOT, 0, "vendor/framework/*"),
    rule(0o644, AID_ROOT, AID_ROOT, 0, "vendor/app/*"),
    rule(0o644, AID_ROOT, AID_ROOT, 0, "vendor/priv-app/*"),
];

/// The rules for files and for directories, in the order they are tried.
pub struct FsConfig {
    files: Vec<Rule>,
    dirs: Vec<Rule>,
}

impl FsConfig {
    /// The partitions' `fs_config_files` and `fs_config_dirs` contents, in the
    /// order Android reads them: system, vendor, oem, odm, product, system_ext.
    pub fn new(files: &[&[u8]], dirs: &[&[u8]]) -> Result<Self, Error> {
        let parsed =
            |contents: &[&[u8]], table: &[(Ownership, &str)]| -> Result<Vec<Rule>, Error> {
                let mut rules = Vec::new();
                for content in contents {
                    rules.extend(read(content)?);
                }
                rules.extend(table.iter().map(|(ownership, prefix)| Rule {
                    prefix: (*prefix).to_owned(),
                    ownership: *ownership,
                }));
                Ok(rules)
            };
        Ok(Self {
            files: parsed(files, FILES)?,
            dirs: parsed(dirs, DIRS)?,
        })
    }

    /// What `fs_config` gives `path`, a device path without its leading `/`.
    pub fn get(&self, path: &str, dir: bool) -> Ownership {
        let rules = if dir { &self.dirs } else { &self.files };
        rules
            .iter()
            .find(|rule| matches(dir, &rule.prefix, path))
            .map_or(
                Ownership {
                    uid: AID_ROOT,
                    gid: AID_ROOT,
                    mode: if dir { 0o755 } else { 0o644 },
                    capabilities: 0,
                },
                |rule| rule.ownership,
            )
    }
}

/// The entries of an `fs_config_*` file: a 16-byte little-endian header (length of
/// the whole entry, mode, uid, gid, capabilities) and a NUL-terminated prefix.
fn read(content: &[u8]) -> Result<Vec<Rule>, Error> {
    let mut rules = Vec::new();
    let mut offset = 0;
    while offset + 16 <= content.len() {
        let u16_at = |at: usize| u16::from_le_bytes([content[at], content[at + 1]]);
        let length = usize::from(u16_at(offset));
        let end = offset + length;
        if length <= 16 || end > content.len() {
            return Err(Error::FsConfig(offset));
        }
        let mut capabilities = [0; 8];
        capabilities.copy_from_slice(&content[offset + 8..offset + 16]);
        let prefix = &content[offset + 16..end];
        let prefix = prefix
            .iter()
            .position(|byte| *byte == 0)
            .map(|nul| &prefix[..nul])
            .ok_or(Error::FsConfig(offset))?;
        rules.push(Rule {
            prefix: String::from_utf8_lossy(prefix).into_owned(),
            ownership: Ownership {
                mode: u16_at(offset + 2),
                uid: u32::from(u16_at(offset + 4)),
                gid: u32::from(u16_at(offset + 6)),
                capabilities: u64::from_le_bytes(capabilities),
            },
        });
        offset = end;
    }
    Ok(rules)
}

/// libcutils' `fs_config_cmp`: an `fnmatch` without `FNM_PATHNAME`, where a
/// directory pattern covers what is below it, and `system/vendor/...` (and the
/// other logical partitions under `system/` or `vendor/`) also match as `vendor/...`.
fn matches(dir: bool, prefix: &str, path: &str) -> bool {
    let mut pattern = prefix.to_owned();
    let mut input = path.to_owned();
    if dir {
        if !input.ends_with('/') {
            input.push('/');
        }
        if !pattern.ends_with("/*") {
            pattern.push_str(if pattern.ends_with('/') { "*" } else { "/*" });
        }
    }
    if fnmatch(pattern.as_bytes(), input.as_bytes()) {
        return true;
    }
    const LOGICAL: [&str; 4] = [
        "system/product/",
        "system/system_ext/",
        "system/vendor/",
        "vendor/odm/",
    ];
    const PARTITIONS: [&str; 5] = ["odm/", "oem/", "product/", "system_ext/", "vendor/"];
    LOGICAL.iter().any(|logical| {
        let Some(inner) = input.strip_prefix(&logical[..logical.find('/').unwrap_or(0) + 1]) else {
            return false;
        };
        input.starts_with(logical)
            && PARTITIONS
                .iter()
                .any(|partition| inner.starts_with(partition))
            && fnmatch(pattern.as_bytes(), inner.as_bytes())
    })
}

/// `fnmatch` with `FNM_NOESCAPE` only: `*` matches anything, `/` included.
fn fnmatch(pattern: &[u8], input: &[u8]) -> bool {
    match pattern.split_first() {
        None => input.is_empty(),
        Some((b'*', rest)) => (0..=input.len()).any(|skip| fnmatch(rest, &input[skip..])),
        Some((b'?', rest)) => !input.is_empty() && fnmatch(rest, &input[1..]),
        Some((b'[', rest)) => match (bracket(rest, input.first().copied()), input.split_first()) {
            (Some((true, after)), Some((_, input))) => fnmatch(after, input),
            (Some((false, _)), _) | (Some(_), None) => false,
            // An unterminated `[` is an ordinary character.
            (None, _) => input.first() == Some(&b'[') && fnmatch(rest, &input[1..]),
        },
        Some((literal, rest)) => input.first() == Some(literal) && fnmatch(rest, &input[1..]),
    }
}

/// A bracket expression after its `[`: whether `byte` is in it, and the pattern
/// after its `]`; `None` when it has no `]`.
fn bracket(pattern: &[u8], byte: Option<u8>) -> Option<(bool, &[u8])> {
    let (negated, mut rest) = match pattern.first() {
        Some(b'!' | b'^') => (true, &pattern[1..]),
        _ => (false, pattern),
    };
    let mut found = false;
    let mut first = true;
    loop {
        match rest {
            [] => return None,
            [b']', after @ ..] if !first => {
                return Some((found != negated && byte.is_some(), after));
            }
            [low, b'-', high, after @ ..] if *high != b']' => {
                found |= byte.is_some_and(|byte| (*low..=*high).contains(&byte));
                rest = after;
            }
            [single, after @ ..] => {
                found |= byte == Some(*single);
                rest = after;
            }
        }
        first = false;
    }
}

/// The `security.capability` xattr for `capabilities`: `vfs_cap_data` revision 2,
/// effective, as Android's image tools write it; `None` for no capabilities.
pub fn capability_xattr(capabilities: u64) -> Option<Vec<u8>> {
    if capabilities == 0 {
        return None;
    }
    const REVISION_2_EFFECTIVE: u32 = 0x0200_0001;
    let mut value = Vec::with_capacity(20);
    for word in [
        REVISION_2_EFFECTIVE,
        capabilities as u32,
        0,
        (capabilities >> 32) as u32,
        0,
    ] {
        value.extend_from_slice(&word.to_le_bytes());
    }
    Some(value)
}
