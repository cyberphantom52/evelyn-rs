//! Android property files (`build.prop`, `default.prop`, ...): reading and editing.
//!
//! Lines are read as init reads them: full-line `#` comments, `import <path>
//! [<filter>]` lines, and `key=value` assignments, the key and value trimmed of
//! whitespace; an inline `#` is part of the value. A later assignment of a key
//! overrides an earlier one. Edits keep every other byte as it was: a replacement
//! preserves the text up to the value and the original line ending, and an append
//! uses the file's own line ending (LF by default).

/// Whether a file at `path` is a property file Android reads (`build.prop`,
/// `default.prop`, ...).
pub fn is_property_file(path: &str) -> bool {
    path.ends_with(".prop")
}

/// Set every assignment of `key`, appending one when there is none.
pub fn set(input: &str, key: &str, value: &str) -> String {
    edit(input, key, Some(value))
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

/// The line ending of `text`'s first line, defaulting to LF.
fn newline(text: &str) -> &str {
    if text
        .split_inclusive('\n')
        .next()
        .is_some_and(|s| s.ends_with("\r\n"))
    {
        "\r\n"
    } else {
        "\n"
    }
}

/// The text before the value of `line`, if it assigns `key`.
fn prefix<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let (entry, prefix) = assignment(line)?;
    (entry.key == key).then_some(prefix)
}

fn edit(input: &str, key: &str, value: Option<&str>) -> String {
    let actual = input
        .split_inclusive('\n')
        .filter(|line| prefix(line, key).is_some())
        .count();
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
    if actual == 0
        && let Some(value) = value
    {
        let eol = newline(input);
        if !output.is_empty() && !output.ends_with('\n') {
            output.push_str(eol);
        }
        output.push_str(key);
        output.push('=');
        output.push_str(value);
        output.push_str(eol);
    }
    output
}
