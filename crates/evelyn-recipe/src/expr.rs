//! The expressions recipes are written with: the primitive tree's, named for how a
//! recipe reads, each desugared into the tree's when the recipe is loaded. A file
//! is named by where it is looked for, a [`Location`], and a string can name a bound value as
//! `{name}`: `Exists(Asset("apex/com.android.vndk.v{vndk}.apex"))`.

use crate::{
    Error,
    location::{Kind, Location},
    tree::Pattern,
};
use android_firmware::Name;
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
    /// A string is one of these: `In(Property(key: "ro.x"), ["a", "b"])`.
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
