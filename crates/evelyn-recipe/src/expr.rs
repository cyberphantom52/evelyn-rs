//! The expressions recipes are written with: the primitive tree's, named for how a
//! recipe reads, each desugared into the tree's when the recipe is loaded. A file
//! is named by where it is looked for, a [`Location`], and a string can name a bound value as
//! `{name}`: `Exists(Asset("apex/com.android.vndk.v{vndk}.apex"))`.

use crate::{
    Error,
    location::{Kind, Location},
    tree::Pattern,
};
use android_firmware::{DevicePath, Name, RelativePath};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Expr {
    /// The text of the one file selected.
    Read(Box<Expr>),
    /// The SHA-256 of the one file selected.
    Hash(Box<Expr>),
    /// The entries selected and everything below them.
    Below(Box<Expr>),
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
    Any(Vec<Expr>),
    /// What any of the selections selects, in the order they select it.
    Union(Vec<Expr>),
    Eq(Box<Expr>, Box<Expr>),
    /// A string is one of these, each of which can name a bound value:
    /// `In(Property(key: "ro.x"), ["a", "{v}"])`.
    In(Box<Expr>, Vec<String>),
    If {
        condition: Box<Expr>,
        then: Box<Expr>,
        otherwise: Box<Expr>,
    },
    /// The strings of a list, or the entries of a selection, for which `keep`
    /// holds with `{name}` bound to each.
    Filter {
        over: Box<Expr>,
        name: Name,
        keep: Box<Expr>,
    },
    /// The lines of a string, as a list.
    Lines(Box<Expr>),
    /// A list's strings, each ending in a newline.
    Join(Box<Expr>),
    /// The string without the whitespace at its ends.
    Trim(Box<Expr>),
    /// The string has a match of the regular expression in it.
    Matches(Box<Expr>, Pattern),
    /// The files a location selects: `Build("/…")`, `Rom("/…")` and so on.
    #[serde(untagged)]
    At(Location),
    /// A string, in which `{name}` is the value `name` is bound to.
    #[serde(untagged)]
    Text(String),
    #[serde(untagged)]
    Bool(bool),
}

impl Expr {
    /// That every location in it is one an expression can select: inside a
    /// package its files, `Package(…)`, outside it the build's, `Build(…)`; a
    /// `Partition(…)` only to test it exists or to read properties from.
    pub(crate) fn check(&self, in_package: bool) -> Result<(), Error> {
        let files: &[Kind] = match in_package {
            true => &[Kind::Package, Kind::Rom, Kind::Stock, Kind::Asset],
            false => &[Kind::Build, Kind::Rom, Kind::Stock, Kind::Asset],
        };
        let check = |expr: &Expr| expr.check(in_package);
        match self {
            Self::At(location) => location.check(files, "an expression"),
            Self::Exists(inner) => match &**inner {
                Self::At(Location::Partition(_)) => Ok(()),
                inner => check(inner),
            },
            Self::Property { target, .. } => target
                .iter()
                .try_for_each(|target| target.check(&[Kind::Build, Kind::Partition], "Property")),
            Self::Read(inner)
            | Self::Hash(inner)
            | Self::Below(inner)
            | Self::Not(inner)
            | Self::Lines(inner)
            | Self::Join(inner)
            | Self::Trim(inner)
            | Self::Matches(inner, _) => check(inner),
            Self::All(exprs) | Self::Any(exprs) | Self::Union(exprs) => {
                exprs.iter().try_for_each(check)
            }
            Self::Eq(left, right) => {
                check(left)?;
                check(right)
            }
            Self::In(inner, _) => check(inner),
            Self::If {
                condition,
                then,
                otherwise,
            } => {
                check(condition)?;
                check(then)?;
                check(otherwise)
            }
            Self::Filter { over, keep, .. } => {
                check(over)?;
                check(keep)
            }
            Self::Text(_) | Self::Bool(_) => Ok(()),
        }
    }
}

/// A piece of a written string: text, or the value a name is bound to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Part {
    Text(String),
    Bound(Name),
}

/// The pieces of a written string. `{name}`, a name of `[A-Za-z0-9_-]`, is the
/// value bound to it; `{{` and `}}` are `{` and `}`; any other brace stands for
/// itself, as in an XML namespace's `{http://…}`.
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
        let bound = (rest.starts_with('{') && end > 0 && after[end..].starts_with('}'))
            .then(|| Name::try_from(after[..end].to_owned()).ok())
            .flatten();
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
pub(crate) fn written_out(written: &str) -> Option<String> {
    match &parts(written)[..] {
        [Part::Text(text)] => Some(text.clone()),
        _ => None,
    }
}

/// Whether `written`, written out, is a valid `T`; one naming a bound value is
/// checked when it is used.
pub(crate) fn written<T>(written: &str) -> Result<(), Error>
where
    T: TryFrom<String>,
    Error: From<T::Error>,
{
    match written_out(written) {
        Some(text) => T::try_from(text).map(drop).map_err(Error::from),
        None => Ok(()),
    }
}

/// Whether `written`, written out, is a build pattern: a device path, or a path
/// relative to each partition a scope selects.
pub(crate) fn pattern(written: &str) -> Result<(), Error> {
    match written_out(written) {
        Some(path) if path.starts_with('/') => Ok(DevicePath::try_from(path).map(drop)?),
        Some(path) => Ok(RelativePath::try_from(path).map(drop)?),
        None => Ok(()),
    }
}

/// Whether `written`, written out, is as `valid` wants it; one naming a bound
/// value is checked when it is used.
pub(crate) fn written_or(written: &str, valid: impl Fn(&str) -> bool) -> bool {
    written_out(written).is_none_or(|written| valid(&written))
}
