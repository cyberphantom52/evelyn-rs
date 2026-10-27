//! What recipe expressions evaluate to, and what each operator means for each kind
//! of value. An operator applied to values it is not defined for is an error, not
//! a silent result.

use crate::Firmware;
use model::{Name, RelativePath};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Value {
    Str(String),
    Bool(bool),
    /// A property that is not set.
    Unset,
    List(Vec<String>),
    /// Entries in the build, a firmware or a package.
    Selection(Vec<Located>),
}

/// One entry a selection holds.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Located {
    pub place: Place,
    pub partition: Name,
    pub path: RelativePath,
}

/// What a selected entry is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Place {
    /// The build, in `partition`.
    Build,
    /// A firmware, in its `partition`.
    Firmware(Firmware),
    /// The package being edited, which is in `partition`.
    Package,
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("{op} is not defined for {found}")]
pub struct ValueError {
    pub op: &'static str,
    pub found: String,
}

impl Value {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Str(_) => "a string",
            Self::Bool(_) => "a boolean",
            Self::Unset => "an unset value",
            Self::List(_) => "a list",
            Self::Selection(_) => "a selection",
        }
    }

    fn undefined(op: &'static str, found: impl Into<String>) -> ValueError {
        ValueError {
            op,
            found: found.into(),
        }
    }

    pub fn as_bool(&self) -> Result<bool, ValueError> {
        match self {
            Self::Bool(value) => Ok(*value),
            other => Err(Self::undefined("a condition", other.kind())),
        }
    }

    /// A string, or `None` when unset.
    pub fn as_str(&self) -> Result<Option<&str>, ValueError> {
        match self {
            Self::Str(value) => Ok(Some(value)),
            Self::Unset => Ok(None),
            other => Err(Self::undefined("a string", other.kind())),
        }
    }

    pub fn as_selection(&self) -> Result<&[Located], ValueError> {
        match self {
            Self::Selection(entries) => Ok(entries),
            other => Err(Self::undefined("a selection", other.kind())),
        }
    }

    /// Equal strings or booleans; an unset value equals nothing.
    pub fn equals(&self, other: &Self) -> Result<Self, ValueError> {
        match (self, other) {
            (Self::Str(a), Self::Str(b)) => Ok(Self::Bool(a == b)),
            (Self::Bool(a), Self::Bool(b)) => Ok(Self::Bool(a == b)),
            (Self::Unset, Self::Str(_) | Self::Unset) | (Self::Str(_), Self::Unset) => {
                Ok(Self::Bool(false))
            }
            (a, b) => Err(Self::undefined(
                "Eq",
                format!("{} and {}", a.kind(), b.kind()),
            )),
        }
    }

    /// A string in a list; an unset value is in no list.
    pub fn is_in(&self, list: &Self) -> Result<Self, ValueError> {
        match (self, list) {
            (Self::Str(value), Self::List(values)) => Ok(Self::Bool(values.contains(value))),
            (Self::Unset, Self::List(_)) => Ok(Self::Bool(false)),
            (a, b) => Err(Self::undefined(
                "In",
                format!("{} and {}", a.kind(), b.kind()),
            )),
        }
    }

    /// A string or selection has something in it.
    pub fn defined(&self) -> Result<Self, ValueError> {
        match self {
            Self::Str(_) => Ok(Self::Bool(true)),
            Self::Unset => Ok(Self::Bool(false)),
            Self::Selection(entries) => Ok(Self::Bool(!entries.is_empty())),
            other => Err(Self::undefined("Defined", other.kind())),
        }
    }
}

impl std::ops::Not for Value {
    type Output = Result<Value, ValueError>;

    fn not(self) -> Self::Output {
        Ok(Value::Bool(
            !self
                .as_bool()
                .map_err(|_| Value::undefined("Not", self.kind()))?,
        ))
    }
}

impl std::ops::BitAnd for Value {
    type Output = Result<Value, ValueError>;

    fn bitand(self, other: Value) -> Self::Output {
        match (&self, &other) {
            (Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(*a && *b)),
            (a, b) => Err(Value::undefined(
                "All",
                format!("{} and {}", a.kind(), b.kind()),
            )),
        }
    }
}

impl std::ops::BitOr for Value {
    type Output = Result<Value, ValueError>;

    fn bitor(self, other: Value) -> Self::Output {
        match (&self, &other) {
            (Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(*a || *b)),
            // Two selections: what either selects.
            (Value::Selection(a), Value::Selection(b)) => {
                let mut union: Vec<Located> = a.iter().chain(b).cloned().collect();
                union.sort();
                union.dedup();
                Ok(Value::Selection(union))
            }
            (a, b) => Err(Value::undefined(
                "Any",
                format!("{} and {}", a.kind(), b.kind()),
            )),
        }
    }
}
