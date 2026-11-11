//! Android property files (`build.prop`, `default.prop`, ...): reading and editing.
//!
//! Lines are read as init reads them: full-line `#` comments, `import <path>
//! [<filter>]` lines, and `key=value` assignments, the key and value trimmed of
//! whitespace; an inline `#` is part of the value. A later assignment of a key
//! overrides an earlier one. Edits keep every other byte as it was: a replacement
//! preserves the text up to the value and the original line ending, and an append
//! uses the file's own line ending.

/// Set every assignment of `key`; with none, append one with `create`, in the
/// file's own line ending (LF by default), else leave the text as it was.
pub fn set(input: &str, key: &str, value: &str, create: bool) -> String {
    let mut output = edit(input, key, Some(value));
    if create && !entries(input).any(|entry| entry.key == key) {
        let eol = newline(input);
        if !output.is_empty() && !output.ends_with('\n') {
            output.push_str(eol);
        }
        output.push_str(&format!("{key}={value}{eol}"));
    }
    output
}

/// The line ending of `text`'s first line, defaulting to LF.
fn newline(text: &str) -> &str {
    match text.split_inclusive('\n').next() {
        Some(line) if line.ends_with("\r\n") => "\r\n",
        _ => "\n",
    }
}

/// Remove every complete assignment line of `key`, including its line ending.
pub fn remove(input: &str, key: &str) -> String {
    edit(input, key, None)
}

/// One assignment, in file order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry<'a> {
    pub key: &'a str,
    pub value: &'a str,
}

/// What a line is to init.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Line<'a> {
    Assignment(Entry<'a>),
    /// Load the file at `path`, a device path, here; with a filter, only its keys
    /// equal to the filter or, for `prefix.*`, starting with `prefix.`.
    Import {
        path: &'a str,
        filter: Option<&'a str>,
    },
}

/// The assignments and imports in `text`, in order.
pub fn lines(text: &str) -> impl Iterator<Item = Line<'_>> {
    text.split_inclusive('\n').filter_map(|line| {
        if let Some(import) = body(line).trim().strip_prefix("import ") {
            let (path, filter) = match import.trim_start().split_once(' ') {
                Some((path, filter)) => (path, Some(filter.trim_start())),
                None => (import.trim_start(), None),
            };
            return Some(Line::Import { path, filter });
        }
        assignment(line).map(|(entry, _)| Line::Assignment(entry))
    })
}

/// Every assignment in `text`, in order.
pub fn entries(text: &str) -> impl Iterator<Item = Entry<'_>> {
    lines(text).filter_map(|line| match line {
        Line::Assignment(entry) => Some(entry),
        Line::Import { .. } => None,
    })
}

/// Whether `filter`, an import's, lets `key` in.
pub fn filtered(filter: &str, key: &str) -> bool {
    match filter.strip_suffix('*') {
        Some(prefix) => key.starts_with(prefix),
        None => key == filter,
    }
}

/// The assignment on `line`, if it is one, and the text before its value.
fn assignment(line: &str) -> Option<(Entry<'_>, &str)> {
    let line = body(line);
    let start = line.trim_start();
    if start.starts_with('#') || start.starts_with("import ") {
        return None;
    }
    let (left, right) = line.split_once('=')?;
    let key = left.trim();
    let right = right.trim_start();
    let prefix = &line[..line.len() - right.len()];
    (!key.is_empty()).then_some((
        Entry {
            key,
            value: right.trim_end(),
        },
        prefix,
    ))
}

/// The value `key` ends up with after loading `text` alone: its last assignment.
pub fn get<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    entries(text)
        .filter(|entry| entry.key == key)
        .last()
        .map(|entry| entry.value)
}

/// `line` without its LF or CRLF ending.
fn body(line: &str) -> &str {
    line.strip_suffix("\r\n")
        .or_else(|| line.strip_suffix('\n'))
        .unwrap_or(line)
}

/// The text before the value of `line`, if it assigns `key`.
fn prefix<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let (entry, prefix) = assignment(line)?;
    (entry.key == key).then_some(prefix)
}

fn edit(input: &str, key: &str, value: Option<&str>) -> String {
    let mut output = String::with_capacity(input.len());
    for line in input.split_inclusive('\n') {
        match prefix(line, key) {
            Some(prefix) => {
                if let Some(value) = value {
                    output.push_str(prefix);
                    output.push_str(value);
                    output.push_str(&line[body(line).len()..]);
                }
            }
            None => output.push_str(line),
        }
    }
    output
}

/// The property files init loads at boot, in the order it loads them, an assignment
/// in a later one overriding one in an earlier one. Of a group of several, init
/// loads the first if the device has it, else the legacy files after it.
pub const PROPERTY_FILES: [&[&str]; 9] = [
    &["/system/build.prop"],
    &[
        "/system_ext/etc/build.prop",
        "/system_ext/default.prop",
        "/system_ext/build.prop",
    ],
    &["/system_dlkm/etc/build.prop"],
    &["/vendor/default.prop"],
    &["/vendor/build.prop"],
    &["/vendor_dlkm/etc/build.prop"],
    &["/odm_dlkm/etc/build.prop"],
    &[
        "/odm/etc/build.prop",
        "/odm/default.prop",
        "/odm/build.prop",
    ],
    &[
        "/product/etc/build.prop",
        "/product/default.prop",
        "/product/build.prop",
    ],
];

/// Where in a partition its own properties are, the one a new key goes to first.
pub const PARTITION_PROPERTY_FILES: [&str; 3] =
    ["system/build.prop", "etc/build.prop", "build.prop"];

/// Keys init does not take from a property file.
pub fn loadable_property(key: &str) -> bool {
    !key.starts_with("ctl.") && key != "sys.powerctl" && key != "selinux.restorecon_recursive"
}

/// The `ro.product.*` properties init derives, when a property file leaves one unset
/// or empty, from the first of `ro.product.<source>.*` that is set, by
/// `ro.product.property_source_order` or, without it or with a source not listed
/// here, in this order of sources.
pub const DERIVED_PRODUCT_PROPERTIES: [&str; 5] =
    ["brand", "device", "manufacturer", "model", "name"];
pub const PRODUCT_PROPERTY_SOURCES: [&str; 5] =
    ["product", "odm", "vendor", "system_ext", "system"];
