//! The expressions recipes are written with, in `let:` and `when:`. They desugar
//! into the primitive tree's expressions, where their types are checked.

use crate::PropertyTarget;
use android_firmware::{DevicePath, Name};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Expr {
    /// A property's value, if set. Without a target, the value the device ends up
    /// with; with a partition, from any `.prop` file in it; with a file, from it.
    Property {
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<PropertyTarget>,
    },
    /// The string expression has a value.
    Defined(Box<Expr>),
    /// The string expression has this value.
    Eq(Box<Expr>, String),
    /// The string expression has one of these values.
    In(Box<Expr>, Vec<String>),
    /// An entry, or one matching a glob, exists in the build.
    Exists(DevicePath),
    /// The build has this partition.
    HasPartition(Name),
    Not(Box<Expr>),
    All(Vec<Expr>),
    Any(Vec<Expr>),
}
