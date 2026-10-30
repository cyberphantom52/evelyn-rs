//! Android property files (`build.prop`, `default.prop`, ...): reading and editing.
//!
//! Lines are full-line `#` comments or `key=value` / `key?=value` assignments; an
//! inline `#` is part of the value. Edits keep every other byte as it was: a
//! replacement preserves the text up to the value and the original line ending, and
//! an append uses the file's own line ending (LF by default).

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("property match count: expected {expected}, found {actual}")]
    Count { expected: usize, actual: usize },
    #[error("property {0} is not defined")]
    Undefined(String),
}

/// Whether a file at `path` is a property file Android reads (`build.prop`,
/// `default.prop`, ...).
pub fn is_property_file(path: &str) -> bool {
    path.ends_with(".prop")
}

/// Set every assignment of `key`, appending one when there is none. `expect`
/// pins how many assignments there must be.
pub fn set(input: &str, key: &str, value: &str, expect: Option<usize>) -> Result<String, Error> {
    edit(input, key, Some(value), expect)
}

/// Remove every complete assignment line of `key`, including its line ending.
/// `expect` pins how many there must be; without it, at least one.
pub fn remove(input: &str, key: &str, expect: Option<usize>) -> Result<String, Error> {
    edit(input, key, None, expect)
}

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

fn prefix<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let line = body(line);
    if line.trim_start().starts_with('#') {
        return None;
    }
    let (left, right) = line.split_once('=')?;
    let left = left.trim();
    if left.strip_suffix('?').unwrap_or(left).trim_end() != key {
        return None;
    }
    let whitespace = right.len() - right.trim_start_matches([' ', '\t']).len();
    Some(&line[..line.len() - right.len() + whitespace])
}

fn edit(
    input: &str,
    key: &str,
    value: Option<&str>,
    expect: Option<usize>,
) -> Result<String, Error> {
    let actual = input
        .split_inclusive('\n')
        .filter(|line| prefix(line, key).is_some())
        .count();
    match expect {
        Some(expected) if actual != expected => {
            return Err(Error::Count { expected, actual });
        }
        None if actual == 0 && value.is_none() => return Err(Error::Undefined(key.into())),
        _ => {}
    }
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
    Ok(output)
}
