//! The primitive tree recipes are applied as. Recipes are written with the surface
//! operations (`Remove`, `Copy`, `SetProperty`, ...); loading desugars them into
//! these few statements and expressions, so each primitive has one implementation
//! however many surface forms use it.

use crate::{Firmware, Hex, Returns, XmlEdit, value::Value};
use android_firmware::Name;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A recipe as statements, its steps in the order they apply.
#[derive(Clone, Debug)]
pub struct Tree {
    pub name: Name,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug)]
/// A statement over an empty selection does nothing; where that would be a
/// mistake, the surface operation asserts the selection first.
pub enum Stmt {
    /// A named unit: what provenance and skip reports refer to.
    Step { id: Name, body: Vec<Stmt> },
    /// Bind `name` to a value for the body's expressions. A string bound here must
    /// be a token, `[A-Za-z0-9._-]`: it comes from firmware and goes into paths.
    Let {
        name: Binding,
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
    /// `metadata`. Writing in place of an entry needs `overwrite`, and making a
    /// missing parent directory `create_parent`; without them it is an error.
    Copy {
        selection: Expr,
        /// A device path.
        to: Option<Expr>,
        metadata: Metadata,
        overwrite: bool,
        create_parent: bool,
    },
    /// Set metadata on every entry selected (not what is below it; for that,
    /// select with `Below`).
    SetMetadata { selection: Expr, metadata: Assigned },
    /// Transform the content of every file selected.
    Edit {
        selection: Expr,
        transform: Transform,
    },
    /// The condition must hold, or the build fails with `message`.
    Assert { condition: Expr, message: Expr },
    /// Decode a package (APK or JAR), run the body inside it and rebuild it. In
    /// the body, `/` is the package's root: `/res/values/strings.xml`.
    Package {
        /// A device path.
        package: Expr,
        body: Vec<Stmt>,
    },
}

#[derive(Clone, Debug)]
pub enum Transform {
    /// Apply the unified diff the asset, a path in the recipe's directory, holds.
    ApplyDiff(Expr),
    /// Write bytes at offsets.
    WriteBytes(BTreeMap<u64, Hex>),
    Xml(Vec<XmlEdit<Expr>>),
    /// Set every assignment of a property to `value`, or remove them without one.
    /// With `create`, a value is appended where the file assigns none.
    Property {
        key: Expr,
        value: Option<Expr>,
        create: bool,
    },
    /// Replace the content with a string.
    Write(Expr),
    /// Change one method of the class a smali file defines.
    Smali {
        class: Expr,
        method: Expr,
        edit: MethodEdit,
    },
    /// Patch one of the device's CIL policy files with TE `policies` (assets), checked
    /// to resolve with the policy files `resolve_with` selects.
    AddPolicy {
        /// Paths in the recipe's directory.
        policies: Vec<Expr>,
        resolve_with: Expr,
    },
}

impl Transform {
    /// The strings it names, as expressions: a value, a path, a name.
    pub fn strings(&self) -> Vec<&Expr> {
        match self {
            Self::ApplyDiff(patch) => vec![patch],
            Self::Write(text) => vec![text],
            Self::Property { key, value, .. } => std::iter::once(key).chain(value).collect(),
            Self::Smali {
                class,
                method,
                edit,
            } => {
                let mut strings = vec![class, method];
                if let MethodEdit::Replace(body) | MethodEdit::Insert(body) = edit {
                    strings.push(body);
                }
                strings
            }
            Self::AddPolicy { policies, .. } => policies.iter().collect(),
            Self::Xml(edits) => edits.iter().flat_map(XmlEdit::strings).collect(),
            Self::WriteBytes(_) => Vec::new(),
        }
    }

    /// The transform with each of its strings replaced by what `resolve` makes of it.
    pub fn map_strings<E>(
        &self,
        mut resolve: impl FnMut(&Expr) -> Result<Expr, E>,
    ) -> Result<Self, E> {
        Ok(match self {
            Self::ApplyDiff(patch) => Self::ApplyDiff(resolve(patch)?),
            Self::Write(text) => Self::Write(resolve(text)?),
            Self::Xml(edits) => Self::Xml(
                edits
                    .iter()
                    .map(|edit| edit.map(&mut resolve))
                    .collect::<Result<_, _>>()?,
            ),
            Self::Property { key, value, create } => Self::Property {
                key: resolve(key)?,
                value: value.as_ref().map(&mut resolve).transpose()?,
                create: *create,
            },
            Self::Smali {
                class,
                method,
                edit,
            } => Self::Smali {
                class: resolve(class)?,
                method: resolve(method)?,
                edit: match edit {
                    MethodEdit::Replace(body) => MethodEdit::Replace(resolve(body)?),
                    MethodEdit::Insert(body) => MethodEdit::Insert(resolve(body)?),
                    MethodEdit::Stub(returns) => MethodEdit::Stub(*returns),
                },
            },
            Self::AddPolicy {
                policies,
                resolve_with,
            } => Self::AddPolicy {
                policies: policies
                    .iter()
                    .map(&mut resolve)
                    .collect::<Result<_, _>>()?,
                resolve_with: resolve_with.clone(),
            },
            other => other.clone(),
        })
    }
}

/// A name a `Let` or `Filter` binds: one a recipe writes, or one the desugaring
/// makes, which no recipe can write, so no string a recipe writes can name it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Binding {
    Written(Name),
    Made(String),
}

impl std::fmt::Display for Binding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Written(name) => f.write_str(name.as_ref()),
            Self::Made(name) => write!(f, "({name})"),
        }
    }
}

/// The primitive expressions. Recipes write the ones in [`crate::Expr`], which
/// desugar into these.
#[derive(Clone, Debug)]
pub enum Expr {
    /// What a statement selects. A pattern starting with `/` is a device path; a
    /// relative one is tried in the directory of every partition the scope selects.
    Select {
        pattern: Box<Expr>,
        scope: Scope,
    },
    /// A property's value, or unset. Without `files`, the value the device ends up
    /// with: init's last assignment of it from the files `PropertyFiles` lists, or
    /// for an `ro.product.*` property init derives, the one it derives. With
    /// `files`, a selection, the last assignment in them, in the selection's order.
    Property {
        key: Box<Expr>,
        files: Option<Box<Expr>>,
    },
    /// The property files init loads, in the order it loads them, the files their
    /// `import` lines name included; with a partition, those in it. An import whose
    /// path holds a `${property}` is not followed: only the device can expand it.
    PropertyFiles {
        partition: Option<Name>,
    },
    /// The value an enclosing `Let` or `Filter` binds to the name.
    Var(Binding),
    /// `body`, with `name` bound to `value`.
    Let {
        name: Binding,
        value: Box<Expr>,
        body: Box<Expr>,
    },
    /// The strings of a list, or the entries of a selection, for which `keep`
    /// holds when `name` is bound to each: a string, or a selection of the one entry.
    Filter {
        over: Box<Expr>,
        name: Binding,
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
#[derive(Clone, Debug)]
pub enum MethodEdit {
    /// Make it only return this.
    Stub(Returns),
    /// Replace it with the asset `body`, one complete method.
    Replace(Expr),
    /// Add the asset `body` as a new method.
    Insert(Expr),
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
#[derive(Clone, Debug)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// Where a selection looks.
#[derive(Clone, Debug, Default)]
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

/// A label as SELinux writes one: `user:role:type:level`, without whitespace.
pub fn selinux_label(label: &str) -> bool {
    label.splitn(4, ':').filter(|part| !part.is_empty()).count() == 4
        && !label.contains(|c: char| c == '\0' || c.is_whitespace())
}

/// The metadata `SetMetadata` sets; what it leaves unset stays.
#[derive(Clone, Debug, Default)]
pub struct Overrides {
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub mode: Option<u16>,
    /// A label, written out or naming a bound value.
    pub selinux: Option<Expr>,
}

impl Overrides {
    /// What it sets is something, and well formed as far as it is written out.
    pub(crate) fn validate(&self) -> Result<(), crate::Error> {
        if self.uid.is_none() && self.gid.is_none() && self.mode.is_none() && self.selinux.is_none()
        {
            return Err(crate::Error::Invalid("metadata that sets nothing".into()));
        }
        if self.mode.is_some_and(|mode| mode > 0o7777) {
            return Err(crate::Error::Invalid(format!(
                "invalid mode {:?}",
                self.mode
            )));
        }
        match self.selinux.as_ref().and_then(Expr::literal_str) {
            Some(label) if !selinux_label(label) => Err(crate::Error::Invalid(format!(
                "invalid SELinux label {label:?}"
            ))),
            _ => Ok(()),
        }
    }
}
