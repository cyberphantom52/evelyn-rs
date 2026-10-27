//! The primitive tree recipes are applied as. Recipes are written with the surface
//! operations (`Remove`, `Import`, `SetProperty`, ...); loading desugars them into
//! these few statements and expressions, so each primitive has one implementation
//! however many surface forms use it.

use crate::{Firmware, Hex, Overrides, PropertyTarget, Returns, XmlEdit, value::Value};
use model::{DevicePath, Name, RelativePath, Sha256Digest};
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
    Delete { selection: Expr },
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
    Within {
        package: DevicePath,
        before: Option<Sha256Digest>,
        body: Vec<Stmt>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Transform {
    TextPatch {
        patch: RelativePath,
        before: Option<Sha256Digest>,
    },
    Bytes {
        before: Sha256Digest,
        edits: BTreeMap<u64, Hex>,
    },
    Xml {
        before: Option<Sha256Digest>,
        edits: Vec<XmlEdit>,
    },
    /// Set a property to `value`, or remove it without one.
    Prop { key: String, value: Option<String> },
    /// Add a string at the end, on a line of its own.
    Append(Expr),
    /// Change one method of the class a smali file defines.
    Smali {
        class: String,
        method: String,
        edit: MethodEdit,
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
    Prop {
        key: String,
        target: Option<PropertyTarget>,
    },
    /// The property files an edit of `key` lands in: where it is defined (in every
    /// partition when `every`), else, when `create`, the file a new key goes to.
    PropFiles {
        key: String,
        target: Option<PropertyTarget>,
        create: bool,
        every: bool,
    },
    /// The entries selected and everything below them.
    Below(Box<Expr>),
    /// The text of the one file selected, in the build or a firmware.
    Content(Box<Expr>),
    /// The files selected whose content contains `text`.
    Containing {
        selection: Box<Expr>,
        text: Box<Expr>,
    },
    /// The SHA-256 of the one file selected.
    Hash(Box<Expr>),
    /// The build has this partition.
    Partition(Name),
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
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

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum UnaryOp {
    Not,
    /// A string is set, or a selection is not empty.
    Defined,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum BinaryOp {
    And,
    Or,
    Eq,
    In,
}
