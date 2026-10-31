//! The expressions recipes are written with: the primitive tree's, named for how a
//! recipe reads, each desugared into the tree's when the recipe is loaded. A file
//! is named by where it is looked for, and a string can name a bound value as
//! `{name}`: `Exists(Asset("apex/com.android.vndk.v{vndk}.apex"))`.

use crate::{
    PropertyTarget,
    tree::{Pattern, Source},
};
use android_firmware::Name;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Expr {
    /// What a pattern matches in the build: a device path, globs allowed. A
    /// relative pattern is tried in the directory of every partition.
    Build(String),
    /// What a device path, globs allowed, matches in the ROM.
    Rom(String),
    /// What a device path, globs allowed, matches in the stock firmware.
    Stock(String),
    /// A file or directory in the recipe's directory, by its path there.
    Asset(String),
    /// The text of the one file selected.
    Read(Box<Expr>),
    /// The SHA-256 of the one file selected.
    Hash(Box<Expr>),
    /// The entries selected and everything below them.
    Below(Box<Expr>),
    /// A property's value, or unset: without a target the value the device ends
    /// up with, as init loads it; with a partition or a file, from its files.
    Property {
        key: String,
        #[serde(default)]
        target: Option<PropertyTarget>,
    },
    /// The build has this partition.
    HasPartition(Name),
    /// A string is set, or a selection is not empty.
    Exists(Box<Expr>),
    Not(Box<Expr>),
    All(Vec<Expr>),
    Any(Vec<Expr>),
    /// What any of the selections selects, in the order they select it.
    Union(Vec<Expr>),
    Eq(Box<Expr>, Box<Expr>),
    /// A string is in a list.
    In(Box<Expr>, Box<Expr>),
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
    /// A string, in which `{name}` is the value `name` is bound to.
    #[serde(untagged)]
    Text(String),
    #[serde(untagged)]
    Bool(bool),
    #[serde(untagged)]
    List(Vec<String>),
}

impl Expr {
    /// Where a file reference looks, the build being `None`, and its pattern.
    pub fn file(&self) -> Option<(Option<Source>, &str)> {
        match self {
            Self::Build(path) => Some((None, path)),
            Self::Rom(path) => Some((Some(Source::Rom), path)),
            Self::Stock(path) => Some((Some(Source::Stock), path)),
            Self::Asset(path) => Some((Some(Source::Assets), path)),
            _ => None,
        }
    }
}
