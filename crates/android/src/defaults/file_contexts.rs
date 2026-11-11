//! SELinux `file_contexts`, as libselinux labels with them: every partition's
//! file in the order Android loads them, each line a regular expression matched
//! against the whole path, an optional file kind and a context. Specs without
//! regular-expression characters win over those with; otherwise the last match in
//! load order wins.

use super::{Error, FileKind};
use regex::Regex;

struct Spec {
    regex: Regex,
    kind: Option<FileKind>,
    /// `None` for `<<none>>`: the path is not to be labelled.
    context: Option<String>,
}

pub struct FileContexts {
    /// Specs with regular-expression characters, then those without, each in load
    /// order: libselinux's sort. Lookups go from the end.
    specs: Vec<Spec>,
}

impl FileContexts {
    /// Every partition's `file_contexts`, in the order Android loads them: plat,
    /// system_ext, product, vendor, odm.
    pub fn new(files: &[&str]) -> Result<Self, Error> {
        let mut regexes = Vec::new();
        let mut literals = Vec::new();
        let mut number = 0;
        for file in files {
            for line in file.lines() {
                number += 1;
                let invalid = |message: String| Error::FileContexts {
                    line: number,
                    message,
                };
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let fields: Vec<&str> = line.split_whitespace().collect();
                let (path, kind, context) = match fields[..] {
                    [path, context] => (path, None, context),
                    [path, kind, context] => (path, Some(kind), context),
                    _ => return Err(invalid(format!("expected 2 or 3 fields: {line}"))),
                };
                let kind = kind
                    .map(|kind| match kind {
                        "--" => Ok(FileKind::File),
                        "-d" => Ok(FileKind::Directory),
                        "-l" => Ok(FileKind::Symlink),
                        "-c" => Ok(FileKind::CharDevice),
                        "-b" => Ok(FileKind::BlockDevice),
                        "-s" => Ok(FileKind::Socket),
                        "-p" => Ok(FileKind::Pipe),
                        other => Err(invalid(format!("unknown file kind {other}"))),
                    })
                    .transpose()?;
                let regex = Regex::new(&format!("^(?:{path})$"))
                    .map_err(|error| invalid(format!("{path}: {error}")))?;
                let spec = Spec {
                    regex,
                    kind,
                    context: (context != "<<none>>").then(|| context.to_owned()),
                };
                if has_meta(path) {
                    regexes.push(spec);
                } else {
                    literals.push(spec);
                }
            }
        }
        regexes.extend(literals);
        Ok(Self { specs: regexes })
    }

    /// The context for `path`, a device path like `/vendor/bin/foo`; `None` when
    /// nothing matches or the match is `<<none>>`.
    pub fn label(&self, path: &str, kind: FileKind) -> Option<&str> {
        self.specs
            .iter()
            .rev()
            .find(|spec| spec.kind.is_none_or(|k| k == kind) && spec.regex.is_match(path))
            .and_then(|spec| spec.context.as_deref())
    }
}

/// libselinux's `spec_hasMetaChars`: an unescaped regular-expression character.
fn has_meta(path: &str) -> bool {
    let mut chars = path.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '.' | '^' | '$' | '?' | '*' | '+' | '|' | '[' | '(' | '{' => return true,
            _ => {}
        }
    }
    false
}

/// The `file_contexts` files, in the order Android loads them.
pub const FILE_CONTEXTS: [&str; 5] = [
    "/system/etc/selinux/plat_file_contexts",
    "/system_ext/etc/selinux/system_ext_file_contexts",
    "/product/etc/selinux/product_file_contexts",
    "/vendor/etc/selinux/vendor_file_contexts",
    "/odm/etc/selinux/odm_file_contexts",
];
