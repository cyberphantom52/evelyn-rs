//! The expressions recipes are written with: conditions, the strings `let:` binds
//! and the values of operations' fields. A file is named by where it is looked
//! for, a [`Location`], and a string can name a bound value as `{name}`:
//! `Exists(Asset("apex/com.android.vndk.v{vndk}.apex"))`.

use crate::recipe::{Error, location::Location};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Expr {
    /// The text of the one file selected.
    Read(Box<Expr>),
    /// A property's value, or unset: without a target the value the device ends
    /// up with, as init loads it; with `Partition(…)` or `Build(…)`, from its files.
    Property {
        key: String,
        #[serde(default)]
        target: Option<Location>,
    },
    /// A string is set, a selection is not empty, or the build has a `Partition(…)`.
    Exists(Box<Expr>),
    Not(Box<Expr>),
    All(Vec<Expr>),
    /// What any of the selections selects, in the order they select it.
    Union(Vec<Expr>),
    /// The strings of a list, or the entries of a selection, for which `keep`
    /// holds with `{name}` bound to each: the string, or the entry's device path.
    Filter {
        over: Box<Expr>,
        name: String,
        keep: Box<Expr>,
    },
    /// The lines of a string, as a list.
    Lines(Box<Expr>),
    /// A list's strings, each ending in a newline.
    Join(Box<Expr>),
    /// The string has a match of the regular expression in it.
    Matches(Box<Expr>, Pattern),
    /// The string has this text in it, which can name a bound value.
    Contains(Box<Expr>, String),
    /// The files a location selects: `Build("/…")`, `Rom("/…")` and so on.
    #[serde(untagged)]
    At(Location),
    /// A string, in which `{name}` is the value `name` is bound to.
    #[serde(untagged)]
    Text(String),
}

impl Expr {
    /// Every location in it, nested ones included.
    pub fn locations(&self) -> Vec<&Location> {
        let mut found = Vec::new();
        self.collect(&mut found);
        found
    }

    fn collect<'a>(&'a self, found: &mut Vec<&'a Location>) {
        match self {
            Self::At(location) => found.push(location),
            Self::Property { target, .. } => found.extend(target),
            Self::Read(inner)
            | Self::Exists(inner)
            | Self::Not(inner)
            | Self::Lines(inner)
            | Self::Join(inner)
            | Self::Matches(inner, _)
            | Self::Contains(inner, _) => inner.collect(found),
            Self::All(exprs) | Self::Union(exprs) => {
                exprs.iter().for_each(|expr| expr.collect(found))
            }
            Self::Filter {
                over: first,
                keep: second,
                ..
            } => {
                first.collect(found);
                second.collect(found);
            }
            Self::Text(_) => {}
        }
    }
}

/// As reports show it: a string as it is, anything else in RON.
impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(text) => f.write_str(text),
            other => f.write_str(&ron::to_string(other).unwrap_or_default()),
        }
    }
}

/// `written` with each `{name}` in it replaced by the value bound to the name.
pub fn substitute(written: &str, bound: &BTreeMap<String, String>) -> Result<String, Error> {
    let mut text = String::new();
    for part in parts(written) {
        match part {
            Part::Text(part) => text.push_str(&part),
            Part::Bound(name) => text.push_str(
                bound
                    .get(&name)
                    .ok_or_else(|| Error::Invalid(format!("{{{name}}} is not bound")))?,
            ),
        }
    }
    Ok(text)
}

/// A piece of a written string: text, or the value a name is bound to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Part {
    Text(String),
    Bound(String),
}

/// The pieces of a written string. `{name}`, a name of `[A-Za-z0-9_-]` starting
/// with a letter or digit, is the value bound to it; `{{` and `}}` are `{` and `}`;
/// any other brace stands for itself, as in an XML namespace's `{http://…}`.
pub(crate) fn parts(written: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut rest = written;
    while let Some(at) = rest.find(['{', '}']) {
        text.push_str(&rest[..at]);
        rest = &rest[at..];
        if let Some(after) = rest.strip_prefix("{{").or_else(|| rest.strip_prefix("}}")) {
            text.push_str(&rest[..1]);
            rest = after;
            continue;
        }
        let after = &rest[1..];
        let end = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .unwrap_or(after.len());
        let bound = (rest.starts_with('{')
            && after.starts_with(|c: char| c.is_ascii_alphanumeric())
            && after[end..].starts_with('}'))
        .then(|| after[..end].to_owned());
        match bound {
            Some(name) => {
                if !text.is_empty() {
                    parts.push(Part::Text(std::mem::take(&mut text)));
                }
                parts.push(Part::Bound(name));
                rest = &after[end + 1..];
            }
            None => {
                text.push_str(&rest[..1]);
                rest = after;
            }
        }
    }
    text.push_str(rest);
    if !text.is_empty() || parts.is_empty() {
        parts.push(Part::Text(text));
    }
    parts
}

/// A written string that names no bound value, as it reads: its escapes undone.
pub fn written_out(written: &str) -> Option<String> {
    match &parts(written)[..] {
        [Part::Text(text)] => Some(text.clone()),
        _ => None,
    }
}

/// A regular expression, compiled when the recipe is loaded.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Pattern(regex::Regex);

impl Pattern {
    pub fn is_match(&self, text: &str) -> bool {
        self.0.is_match(text)
    }
}

impl TryFrom<String> for Pattern {
    type Error = regex::Error;

    fn try_from(pattern: String) -> Result<Self, regex::Error> {
        regex::Regex::new(&pattern).map(Self)
    }
}

impl From<Pattern> for String {
    fn from(pattern: Pattern) -> Self {
        pattern.0.as_str().to_owned()
    }
}
