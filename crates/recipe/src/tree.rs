//! The primitive tree recipes are applied as. Recipes are written with the surface
//! operations (`Remove`, `Import`, `SetProperty`, ...); loading desugars them into
//! these few statements and expressions, so each primitive has one implementation
//! however many surface forms use it.

use crate::{
    ByteEdit, Firmware, MetadataPolicy, MetadataRule, Overrides, PropertyTarget, Returns, XmlEdit,
    value::Value,
};
use model::{DevicePath, Name, RelativePath, Sha256Digest};
use serde::{Deserialize, Serialize};

/// A recipe as statements, its steps in the order they apply.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tree {
    pub name: Name,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Stmt {
    /// A named unit: what provenance and skip reports refer to.
    Step { id: Name, body: Vec<Stmt> },
    /// Bind a string for the body; a value that is unset skips the body.
    Let {
        name: Name,
        value: Expr,
        body: Vec<Stmt>,
    },
    /// Run the body only if the condition holds.
    When { condition: Expr, body: Vec<Stmt> },
    /// Remove every entry selected; `pattern` names the selection in reports.
    Delete { selection: Expr, pattern: String },
    /// Copy the selection into the build, at the same path or at `to`. Overwritten
    /// entries keep the build's metadata and new ones their own; overrides apply
    /// to everything copied.
    Copy {
        selection: Expr,
        to: Option<DevicePath>,
        overrides: Overrides,
    },
    /// Move an entry (and its contents) to another path, within or across
    /// partitions, keeping its metadata.
    Move { from: DevicePath, to: DevicePath },
    /// Write a recipe asset at `to`.
    Write {
        asset: RelativePath,
        sha256: Option<Sha256Digest>,
        to: DevicePath,
        metadata: MetadataPolicy,
    },
    /// Write every file under the asset directory at the same path in `partition`.
    WriteTree {
        assets: RelativePath,
        partition: Name,
        rules: Vec<MetadataRule>,
    },
    /// Transform the content of every file selected.
    Edit {
        selection: Expr,
        transform: Transform,
    },
    /// The condition must hold, or the build fails with `message`.
    Assert { condition: Expr, message: String },
    /// Decode a package (APK or JAR), run the body inside it and rebuild it. In
    /// the body, `/` is the package's root: `/res/values/strings.xml`, and a
    /// `Write`'s metadata is not used.
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
        before: Option<Sha256Digest>,
        edits: Vec<ByteEdit>,
    },
    Xml {
        before: Option<Sha256Digest>,
        edits: Vec<XmlEdit>,
    },
    SetProp {
        key: String,
        value: String,
    },
    DelProp {
        key: String,
    },
    /// Make a smali method only return `returns`.
    StubMethod {
        class: String,
        method: String,
        returns: Returns,
    },
    /// Replace a smali method with the asset `body`.
    ReplaceMethod {
        class: String,
        method: String,
        body: RelativePath,
    },
    /// Add the asset `body` as a new smali method.
    InsertMethod {
        class: String,
        method: String,
        body: RelativePath,
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
    /// The smali file defining a class (`Lcom/example/Name;`), inside a package.
    Class(String),
    /// The SHA-256 of the one file selected.
    Hash(Box<Expr>),
    /// The build has this partition.
    Partition(Name),
    /// A recipe asset exists.
    Asset(RelativePath),
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

/// Where a selection looks.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Scope {
    /// Look inside this firmware instead of the build.
    pub inside: Option<Firmware>,
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
