//! The primitive tree recipes are applied as. Recipes are written with the surface
//! operations (`Remove`, `Import`, `SetProperty`, ...); loading desugars them into
//! these few statements and expressions, so each primitive has one implementation
//! however many surface forms use it.

use crate::{Firmware, Hex, Overrides, PropertyTarget, Returns, XmlEdit, value::Value};
use android_firmware::{DevicePath, Name, RelativePath, Sha256Digest};
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
    /// Bind a string for the body; a value that is unset skips the body.
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
    /// itself) into the build, at the same path or at `to`. Overwritten entries
    /// keep their metadata; new ones have their source's, and assets, which have
    /// none, get Android's defaults.
    Copy {
        selection: Expr,
        to: Option<DevicePath>,
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
        before: Option<Sha256Digest>,
        body: Vec<Stmt>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Transform {
    Diff {
        patch: RelativePath,
        before: Option<Sha256Digest>,
    },
    WriteBytes {
        before: Sha256Digest,
        edits: BTreeMap<u64, Hex>,
    },
    Xml {
        before: Option<Sha256Digest>,
        edits: Vec<XmlEdit>,
    },
    /// Set a property to `value`, or remove it without one.
    Property { key: String, value: Option<String> },
    /// Add a string at the end, on a line of its own.
    Append(Expr),
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Expr {
    Literal(Value),
    /// What a statement selects. A pattern starting with `/` is a device path; a
    /// relative one is tried in the directory of every partition the scope selects.
    Select {
        pattern: String,
        scope: Scope,
    },
    /// A property's value, or unset.
    Property {
        key: String,
        target: Option<PropertyTarget>,
    },
    /// The property files that define `key`: with a file, it if it does; with a
    /// partition, the files in it that do; without, those in every partition.
    PropertyFiles {
        key: String,
        target: Option<PropertyTarget>,
    },
    /// The CIL files init compiles the device's SELinux policy from that the build
    /// has, for the platform policy version the vendor names.
    PolicyFiles,
    /// The file a new property goes to: the file, the partition's `build.prop`,
    /// or without a target product's.
    NewPropertyFile(Option<PropertyTarget>),
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
    /// What any of the selections selects.
    Union(Vec<Expr>),
    /// Two strings, or two booleans, are equal; an unset value equals no string.
    Eq(Box<Expr>, Box<Expr>),
    /// A string is in a list; an unset value is in none.
    In(Box<Expr>, Box<Expr>),
    /// The lines of a string, as a list; an unset value has none.
    Lines(Box<Expr>),
    /// A list's strings, each ending in a newline.
    Join(Box<Expr>),
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

/// What `SetMetadata` gives an entry.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Assigned {
    /// Android's defaults for where it is on the device, worked out from the
    /// finished build's own policy as the device would; fields set later apply
    /// over them.
    Defaults,
    /// These fields; the others stay.
    Fields(Overrides),
}

/// Where a selection can look besides the build.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    Firmware(Firmware),
    /// The recipe's directory: an asset path, relative to it.
    Assets,
}

/// Where a selection looks.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
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
