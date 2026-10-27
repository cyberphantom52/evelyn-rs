//! Android property files (`build.prop`, `default.prop`, ...): reading and editing.
//!
//! Lines are full-line `#` comments or `key=value` / `key?=value` assignments; an
//! inline `#` is part of the value.

/// One assignment, in file order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry<'a> {
    pub key: &'a str,
    pub value: &'a str,
    /// `?=`: applies only when the key is not already set.
    pub optional: bool,
}

/// Every assignment in `text`, in order.
pub fn entries(text: &str) -> impl Iterator<Item = Entry<'_>> {
    text.split_inclusive('\n').filter_map(|line| {
        let line = body(line);
        if line.trim_start().starts_with('#') {
            return None;
        }
        let (left, value) = line.split_once('=')?;
        let left = left.trim();
        let (key, optional) = match left.strip_suffix('?') {
            Some(key) => (key.trim_end(), true),
            None => (left, false),
        };
        (!key.is_empty()).then_some(Entry {
            key,
            value: value.trim_start_matches([' ', '\t']),
            optional,
        })
    })
}

/// The value `key` ends up with after loading `text`, as init applies it.
pub fn get<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    entries(text)
        .filter(|entry| entry.key == key)
        .fold(None, |value, entry| match (value, entry.optional) {
            (Some(value), true) => Some(value),
            _ => Some(entry.value),
        })
}

/// `line` without its LF or CRLF ending.
fn body(line: &str) -> &str {
    line.strip_suffix("\r\n")
        .or_else(|| line.strip_suffix('\n'))
        .unwrap_or(line)
}
