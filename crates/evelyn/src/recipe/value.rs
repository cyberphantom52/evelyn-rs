//! What recipe expressions evaluate to, and what each operator means for each kind
//! of value. An operator applied to values it is not defined for is an error, not
//! a silent result.

use crate::recipe::Firmware;
use android::path::RelativePath;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Located {
    pub place: Place,
    pub partition: String,
    pub path: RelativePath,
}

/// What a selected entry is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Place {
    /// The build, in `partition`.
    Build,
    /// A firmware, in its `partition`.
    Firmware(Firmware),
    /// The package being edited, which is in `partition`.
    Package,
    /// The recipe's assets.
    Asset,
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

    /// A string's lines; an unset value has none.
    pub fn lines(&self) -> Result<Self, ValueError> {
        match self {
            Self::Str(text) => Ok(Self::List(text.lines().map(str::to_owned).collect())),
            Self::Unset => Ok(Self::List(Vec::new())),
            other => Err(Self::undefined("Lines", other.kind())),
        }
    }

    /// A list's strings as lines, each ending in a newline.
    pub fn join(&self) -> Result<Self, ValueError> {
        match self {
            Self::List(lines) => Ok(Self::Str(
                lines
                    .iter()
                    .flat_map(|line| [line.as_str(), "\n"])
                    .collect(),
            )),
            other => Err(Self::undefined("Join", other.kind())),
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
