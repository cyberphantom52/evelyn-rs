//! The primitive tree recipes are applied as. Recipes are written with the surface
//! operations (`Remove`, `Copy`, `SetProperty`, ...); loading desugars them into
//! these few statements and expressions, so each primitive has one implementation
//! however many surface forms use it.

use crate::{Firmware, Hex, Returns, XmlEdit, value::Value};
use android_firmware::{DevicePath, Name, RelativePath};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A recipe as statements, its steps in the order they apply.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tree {
    pub name: Name,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// A statement over an empty selection does nothing; where that would be a
/// mistake, the surface operation asserts the selection first.
pub enum Stmt {
    /// A named unit: what provenance and skip reports refer to.
    Step { id: Name, body: Vec<Stmt> },
    /// Bind `name` to a value for the body's expressions. A string bound here must
    /// be a token, `[A-Za-z0-9._-]`: it comes from firmware and goes into paths.
    Let {
        name: Name,
        value: Expr,
        body: Vec<Stmt>,
    },
    /// Run the body only if the condition holds; otherwise report `skip`.
    When {
        condition: Expr,
        body: Vec<Stmt>,
        skip: String,
    },
    /// Remove every entry selected.
    Remove { selection: Expr },
    /// Copy what is selected (from a firmware, the recipe's assets, or the build
    /// itself) into the build, at the same path or at `to`. Every entry it writes,
    /// new or in place of one, and every directory it makes for them, gets
    /// `metadata`.
    Copy {
        selection: Expr,
        /// A device path.
        to: Option<Expr>,
        metadata: Metadata,
    },
    /// Set metadata on every entry selected (not what is below it; for that,
    /// select with `Below`). `pattern` names the selection in reports.
    SetMetadata {
        selection: Expr,
        pattern: String,
        metadata: Assigned,
    },
    /// Transform the content of every file selected.
    Edit {
        selection: Expr,
        transform: Transform,
    },
    /// The condition must hold, or the build fails with `message`.
    Assert { condition: Expr, message: String },
    /// Decode a package (APK or JAR), run the body inside it and rebuild it. In
    /// the body, `/` is the package's root: `/res/values/strings.xml`.
    Package {
        package: DevicePath,
        body: Vec<Stmt>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Transform {
    /// Apply the unified diff the asset holds.
    ApplyDiff(RelativePath),
    /// Write bytes at offsets.
    WriteBytes(BTreeMap<u64, Hex>),
    Xml(Vec<XmlEdit>),
    /// Set every assignment of a property to `value`, or remove them without one.
    Property {
        key: String,
        value: Option<String>,
    },
    /// Replace the content with a string.
    Write(Expr),
    /// Change one method of the class a smali file defines.
    Smali {
        class: String,
        method: String,
        edit: MethodEdit,
    },
    /// Patch one of the device's CIL policy files with TE `policies` (assets), checked
    /// to resolve with the policy files `resolve_with` selects, less the one patched.
    AddPolicy {
        policies: Vec<RelativePath>,
        resolve_with: Expr,
    },
}

/// The primitive expressions. Recipes write the ones in [`crate::Expr`], which
/// desugar into these.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Expr {
    /// What a statement selects. A pattern starting with `/` is a device path; a
    /// relative one is tried in the directory of every partition the scope selects.
    Select {
        pattern: Box<Expr>,
        #[serde(default)]
        scope: Scope,
    },
    /// A property's value, or unset. Without `files`, the value the device ends up
    /// with: init's last assignment of it from the files `PropertyFiles` lists, or
    /// for an `ro.product.*` property init derives, the one it derives. With
    /// `files`, a selection, the last assignment in them, in the selection's order.
    Property {
        key: String,
        #[serde(default)]
        files: Option<Box<Expr>>,
    },
    /// The property files init loads, in the order it loads them, the files their
    /// `import` lines name included; with a partition, those in it. An import whose
    /// path holds a `${property}` is not followed: only the device can expand it.
    PropertyFiles {
        #[serde(default)]
        partition: Option<Name>,
    },
    /// The value an enclosing `Let` or `Filter` binds to the name.
    Var(Name),
    /// `body`, with `name` bound to `value`.
    Let {
        name: Name,
        value: Box<Expr>,
        body: Box<Expr>,
    },
    /// The strings of a list, or the entries of a selection, for which `keep`
    /// holds when `name` is bound to each: a string, or a selection of the one entry.
    Filter {
        over: Box<Expr>,
        name: Name,
        keep: Box<Expr>,
    },
    /// `then` if the condition holds, else `otherwise`.
    If {
        condition: Box<Expr>,
        then: Box<Expr>,
        otherwise: Box<Expr>,
    },
    /// The entries selected and everything below them.
    Below(Box<Expr>),
    /// The text of the one file selected, in the build or a firmware.
    Read(Box<Expr>),
    /// The string has a match of `pattern` in it; an unset value has none.
    Matches {
        text: Box<Expr>,
        pattern: Pattern,
    },
    /// The SHA-256 of the one file selected.
    Hash(Box<Expr>),
    /// The build has this partition.
    HasPartition(Name),
    Not(Box<Expr>),
    /// A string is set, or a selection is not empty.
    Exists(Box<Expr>),
    /// Every condition holds, checked in order until one does not.
    All(Vec<Expr>),
    /// Some condition holds, checked in order until one does.
    Any(Vec<Expr>),
    /// What any of the selections selects, in the order they select it.
    Union(Vec<Expr>),
    /// Two strings, or two booleans, are equal; an unset value equals no string.
    Eq(Box<Expr>, Box<Expr>),
    /// The strings, one after another.
    Concat(Vec<Expr>),
    /// The string without the whitespace at its ends; an unset value stays unset.
    Trim(Box<Expr>),
    /// The lines of a string, as a list; an unset value has none.
    Lines(Box<Expr>),
    /// A list's strings, each ending in a newline.
    Join(Box<Expr>),
    #[serde(untagged)]
    Literal(Value),
}

impl Expr {
    /// The string a literal is.
    pub fn literal_str(&self) -> Option<&str> {
        match self {
            Self::Literal(Value::Str(text)) => Some(text),
            _ => None,
        }
    }
}

/// How `Smali` changes a method.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum MethodEdit {
    /// Make it only return this.
    Stub(Returns),
    /// Replace it with the asset `body`, one complete method.
    Replace(RelativePath),
    /// Add the asset `body` as a new method.
    Insert(RelativePath),
}

/// What a copy gives what it writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Metadata {
    /// What its source has: the firmware's or the build's, the same directory's
    /// for a directory made. A source without it (an asset, or no such
    /// directory) is an error.
    Source,
    /// Android's defaults for where it is on the device.
    Default,
}

/// What `SetMetadata` gives an entry.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Assigned {
    /// Android's defaults for where it is on the device, worked out from the
    /// finished build's own policy as the device would; fields set later apply
    /// over them.
    Default,
    /// These fields; the others stay.
    Fields(Overrides),
}

/// Where a selection can look besides the build: a firmware, or the recipe's
/// directory, where a path is relative to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    Rom,
    Stock,
    Assets,
    /// The package a `Package` has open, from its root.
    Package,
}

impl Source {
    /// The firmware it is, if it is one.
    pub fn firmware(self) -> Option<Firmware> {
        match self {
            Self::Rom => Some(Firmware::Rom),
            Self::Stock => Some(Firmware::Stock),
            Self::Assets | Self::Package => None,
        }
    }
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Rom => "the ROM",
            Self::Stock => "the stock firmware",
            Self::Assets => "the recipe's assets",
            Self::Package => "the open package",
        })
    }
}

impl From<Firmware> for Source {
    fn from(firmware: Firmware) -> Self {
        match firmware {
            Firmware::Rom => Self::Rom,
            Firmware::Stock => Self::Stock,
        }
    }
}

/// Where a selection looks.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Scope {
    /// Look inside a firmware, or the recipe's assets, instead of the build.
    pub inside: Option<Source>,
    /// Only build partitions that came from this firmware.
    pub origin: Option<Firmware>,
    /// Only partitions matching this name or glob.
    pub partition: Option<String>,
}

/// A regular expression, compiled when the recipe is loaded.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Pattern(regex::Regex);

impl Pattern {
    /// A pattern that matches `text` as it is.
    pub fn literal(text: &str) -> Self {
        Self(regex::Regex::new(&regex::escape(text)).expect("an escaped pattern compiles"))
    }

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

/// The metadata `SetMetadata` sets; what it leaves unset stays.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overrides {
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub mode: Option<u16>,
    pub selinux: Option<String>,
}

impl Overrides {
    pub(crate) fn validate(&self) -> Result<(), crate::Error> {
        if *self == Self::default() {
            return Err(crate::Error::Invalid("metadata that sets nothing".into()));
        }
        if self.mode.is_some_and(|mode| mode > 0o7777) {
            return Err(crate::Error::Invalid(format!(
                "invalid mode {:?}",
                self.mode
            )));
        }
        match &self.selinux {
            Some(label)
                if label.splitn(4, ':').filter(|part| !part.is_empty()).count() != 4
                    || label.contains(|c: char| c == '\0' || c.is_whitespace()) =>
            {
                Err(crate::Error::Invalid(format!(
                    "invalid SELinux label {label:?}"
                )))
            }
            _ => Ok(()),
        }
    }
}
