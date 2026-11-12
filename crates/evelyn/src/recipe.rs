//! The recipe language: steps of operations, applied in dependency order, whose
//! strings can name the values a step binds. A recipe is evaluated as it is
//! written; what is wrong with one is found when it is applied.

mod expr;
pub mod kit;
mod location;
mod xml;
pub use expr::{Expr, substitute, written_out};
pub use location::Location;
pub use xml::{XmlEdit, XmlMatch, XmlName, XmlOp, XmlPredicate, XmlStep, escape_attribute};

use crate::Firmware;
use android::hash::Sha256Digest;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid recipe {path}: {source}")]
    Ron {
        path: PathBuf,
        #[source]
        source: Box<ron::error::SpannedError>,
    },
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

fn read(path: &Path) -> Result<String, Error> {
    fs::read_to_string(path).map_err(|source| Error::Read {
        path: path.into(),
        source,
    })
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub name: String,
    pub steps: Vec<Step>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: String,
    #[serde(default)]
    pub after: Vec<String>,
    /// Names bound to string expressions, evaluated against the tree composed so far,
    /// for the step's condition and operations to use as `{name}` in any string. One
    /// that has no value skips the step.
    #[serde(rename = "let", default)]
    pub bindings: BTreeMap<String, Expr>,
    /// A boolean expression; the step applies only when it holds.
    #[serde(default)]
    pub when: Option<Expr>,
    pub operations: Vec<Operation>,
}

/// Operations change the build: the partitions being produced.
/// Paths are device paths (`/vendor/etc/fstab.qcom`), resolved to a partition by its
/// mount point; `system` is mounted at `/` on system-as-root devices.
///
/// A kit's operation is called as a built-in one is: one with one parameter
/// takes it bare, `DisableFeature("x")`, one with several by name.
#[derive(Clone, Debug, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub enum Operation {
    /// Copy what `from` names into the build, at the same path or at `to`:
    /// `Rom("/…")` or `Stock("/…")`, a device path in a
    /// firmware, globs allowed; `Build("/…")`; or `Asset("…")`, a file or a
    /// directory's files, which needs `to`. Everything it writes, and every
    /// directory it makes, gets `metadata`. What `from` names must be there. It
    /// writes in place of an entry only with `overwrite`, and makes a missing
    /// parent directory only with `create_parent`.
    Copy {
        from: Location,
        /// Where it goes, a device path, if not to the same path. A glob keeps its path.
        #[serde(default)]
        to: Option<Expr>,
        metadata: Metadata,
        #[serde(default)]
        overwrite: bool,
        #[serde(default)]
        create_parent: bool,
    },
    /// Copy what a firmware has, `Rom("/…")` or `Stock("/…")`, to the same path in
    /// the build, with its metadata: `Copy` with `metadata: Source`, `overwrite`
    /// and `create_parent`.
    Import(Location),
    /// Write a recipe asset, a file or a directory's files, at `to`, with Android's
    /// defaults: `Copy` with `metadata: Default`, `overwrite` and `create_parent`.
    Install { asset: String, to: Expr },
    /// Remove what `pattern` matches from the build. A relative pattern is tried in
    /// the directory of every build partition the scope selects: those that came
    /// from the ROM or the stock firmware (`origin`), named by `partition` (a name
    /// or glob), both, or with neither, all of them. A `/...` pattern is a device
    /// path, outside any scope. A pattern that matches nothing is reported, not an
    /// error; a scope that selects no partition is an error.
    Remove {
        pattern: String,
        #[serde(default)]
        origin: Option<Firmware>,
        #[serde(default)]
        partition: Option<String>,
    },
    /// Set metadata Android's defaults do not give, on every entry `pattern` (a
    /// device path, globs allowed) matches in the build.
    SetMetadata {
        pattern: String,
        #[serde(default)]
        uid: Option<u32>,
        #[serde(default)]
        gid: Option<u32>,
        #[serde(default)]
        mode: Option<u16>,
        #[serde(default)]
        selinux: Option<String>,
    },
    /// Set a property everywhere it is defined, else add it. Without a target, in
    /// every property file init loads that defines it, else in the one it loads
    /// last, so the value takes effect; with a partition, in every file of it init
    /// loads that defines it, else its own `build.prop`; with a file, in that file.
    SetProperty {
        key: String,
        value: String,
        #[serde(default)]
        target: Option<Location>,
    },
    /// Remove every definition in the same scope as `SetProperty`. Where none is,
    /// that is reported, not an error.
    RemoveProperty {
        key: String,
        #[serde(default)]
        target: Option<Location>,
    },
    /// Apply a unified diff (an asset) to a file and its hardlinks.
    ApplyDiff { target: String, patch: String },
    /// Overwrite bytes at offsets of a file. What is there decides what the bytes
    /// mean, so an `AssertHash` of the file should come first.
    WriteBytes {
        target: String,
        /// Offsets and the bytes written there, as hex: `{487572: "a9008052"}`.
        edits: BTreeMap<u64, Hex>,
    },
    /// Edit the XML file `target` with `edits`, in order: set an element's text
    /// or an attribute, remove elements, or put a child element under a parent.
    EditXml { target: String, edits: Vec<XmlEdit> },
    /// Decode an APK or JAR, apply `edits` inside it and rebuild it. The edits are
    /// ordinary operations with paths from the package's root
    /// (`/res/values/strings.xml`), plus the smali edits only a package has.
    Package {
        /// A device path, in which `{name}` is a bound value.
        target: String,
        edits: Vec<Operation>,
    },
    /// Inside a `Package`: replace a smali method with the asset `body`.
    ReplaceMethod {
        class: String,
        method: String,
        body: String,
    },
    /// Inside a `Package`: add the asset `body` as a new smali method.
    InsertMethod {
        class: String,
        method: String,
        body: String,
    },
    /// Inside a `Package`: replace a method's body so it only returns `returns`.
    StubMethod {
        class: String,
        method: String,
        returns: Returns,
    },
    /// Inside a `Package`: replace the operand written as `from` with `to` in every
    /// instruction that has it, of every class, or of `class`, or of its `method`:
    /// a method (`Lcom/example/Name;->get()I`), a field, a type, a string in its
    /// quotes, a number. An instruction must have it.
    ReplaceOperand {
        from: String,
        to: String,
        #[serde(default)]
        class: Option<String>,
        #[serde(default)]
        method: Option<String>,
    },
    /// The condition must hold, or the build fails with `message`.
    Assert { condition: Expr, message: String },
    /// The file's content must have this SHA-256.
    AssertHash {
        target: String,
        sha256: Sha256Digest,
    },
    /// Apply `operations` for each string of a list, or each entry of a
    /// selection, with `{name}` bound to it: the string, or the entry's device
    /// path. One over nothing is reported.
    ForEach {
        over: Expr,
        name: String,
        operations: Vec<Operation>,
    },
    /// Apply `operations` if the condition holds. One whose condition does not
    /// hold is not reported.
    When {
        condition: Expr,
        operations: Vec<Operation>,
    },
    /// Add `content` at the end of a file, on a line of its own.
    Append {
        target: String,
        /// A string expression.
        content: Expr,
    },
    /// Patch one of the device's CIL policy files with TE `policies` (assets), written
    /// with Android's policy macros. The result must resolve with the device's other
    /// policy files, as init loads them.
    AddPolicy {
        target: String,
        policies: Vec<String>,
    },
    /// An operation of a kit, by the name the kit gives it, with the strings its
    /// parameters are given.
    #[serde(skip)]
    Call {
        name: String,
        args: BTreeMap<String, String>,
    },
}

/// The built-in operations' names.
pub(crate) const BUILT_IN_OPERATIONS: [&str; 21] = [
    "Copy",
    "Import",
    "Install",
    "Remove",
    "SetMetadata",
    "SetProperty",
    "RemoveProperty",
    "ApplyDiff",
    "WriteBytes",
    "EditXml",
    "Package",
    "ReplaceMethod",
    "InsertMethod",
    "StubMethod",
    "ReplaceOperand",
    "Assert",
    "AssertHash",
    "ForEach",
    "When",
    "Append",
    "AddPolicy",
];

/// What a copy gives what it writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub enum Metadata {
    /// What its source has: the firmware's or the build's, the same directory's
    /// for a directory made. A source without it (an asset, or no such
    /// directory) is an error.
    Source,
    /// Android's defaults for where it is on the device.
    Default,
}

/// What a stubbed method returns: nothing (`V`), zero/false/null, or one/true.
#[derive(Clone, Copy, Debug, Deserialize)]
pub enum Returns {
    Void,
    Zero,
    One,
}

/// Bytes written as hex, two digits each: `"a9008052"`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct Hex(pub Vec<u8>);

impl TryFrom<String> for Hex {
    type Error = String;

    fn try_from(text: String) -> Result<Self, String> {
        android::hash::unhex(&text)
            .filter(|bytes| !bytes.is_empty())
            .map(Self)
            .ok_or_else(|| format!("invalid hex bytes {text:?}"))
    }
}

impl Operation {
    pub fn name(&self) -> &str {
        match self {
            Self::Copy { .. } => "Copy",
            Self::Import(..) => "Import",
            Self::Install { .. } => "Install",
            Self::Remove { .. } => "Remove",
            Self::SetMetadata { .. } => "SetMetadata",
            Self::SetProperty { .. } => "SetProperty",
            Self::RemoveProperty { .. } => "RemoveProperty",
            Self::ApplyDiff { .. } => "ApplyDiff",
            Self::WriteBytes { .. } => "WriteBytes",
            Self::EditXml { .. } => "EditXml",
            Self::Package { .. } => "Package",
            Self::ReplaceMethod { .. } => "ReplaceMethod",
            Self::InsertMethod { .. } => "InsertMethod",
            Self::StubMethod { .. } => "StubMethod",
            Self::ReplaceOperand { .. } => "ReplaceOperand",
            Self::Assert { .. } => "Assert",
            Self::AssertHash { .. } => "AssertHash",
            Self::ForEach { .. } => "ForEach",
            Self::When { .. } => "When",
            Self::Append { .. } => "Append",
            Self::AddPolicy { .. } => "AddPolicy",
            Self::Call { name, .. } => name.as_str(),
        }
    }

    /// What can run on a package's files: content edits, copies of assets and
    /// removals by path, and the smali edits.
    pub fn in_package(&self) -> bool {
        matches!(
            self,
            Self::Remove {
                origin: None,
                partition: None,
                ..
            } | Self::ApplyDiff { .. }
                | Self::WriteBytes { .. }
                | Self::EditXml { .. }
                | Self::Copy { .. }
                | Self::Install { .. }
                | Self::Append { .. }
                | Self::Assert { .. }
                | Self::AssertHash { .. }
                | Self::ReplaceMethod { .. }
                | Self::InsertMethod { .. }
                | Self::StubMethod { .. }
                | Self::ReplaceOperand { .. }
                | Self::ForEach { .. }
                | Self::When { .. }
        )
    }

    /// Every location it names, in its expressions and in the operations inside
    /// it included.
    pub fn locations(&self) -> Vec<&Location> {
        let mut found = Vec::new();
        match self {
            Self::Copy { from, to, .. } => {
                found.push(from);
                found.extend(to.iter().flat_map(Expr::locations));
            }
            Self::Import(from) => found.push(from),
            Self::SetProperty { target, .. } | Self::RemoveProperty { target, .. } => {
                found.extend(target)
            }
            Self::Install { to, .. } => found.extend(to.locations()),
            Self::Append { content, .. } => found.extend(content.locations()),
            Self::Assert { condition, .. } => found.extend(condition.locations()),
            Self::Package { edits, .. } => found.extend(edits.iter().flat_map(Self::locations)),
            Self::ForEach {
                over, operations, ..
            } => {
                found.extend(over.locations());
                found.extend(operations.iter().flat_map(Self::locations));
            }
            Self::When {
                condition,
                operations,
            } => {
                found.extend(condition.locations());
                found.extend(operations.iter().flat_map(Self::locations));
            }
            // What a kit's operation names is in the kit, written out or naming its
            // parameters.
            Self::Call { name, .. } => found.extend(
                kit::OPERATIONS
                    .get()
                    .and_then(|operations| operations.get(name))
                    .into_iter()
                    .flat_map(|definition| definition.operations.iter())
                    .flat_map(Self::locations),
            ),
            _ => {}
        }
        found
    }
}

/// Optional fields are given as plain values, without `Some(...)`, and what a
/// name wraps, a kit operation's parameters by name included, without parentheses
/// of its own.
pub(crate) fn ron_options() -> ron::Options {
    use ron::extensions::Extensions;
    ron::Options::default()
        .with_default_extension(Extensions::IMPLICIT_SOME | Extensions::UNWRAP_VARIANT_NEWTYPES)
}

/// A key a property file can assign.
pub fn property_key(key: &str) -> bool {
    !key.is_empty() && !key.contains(['=', '\n']) && !key.contains(char::is_whitespace)
}

impl Recipe {
    pub fn load(path: &Path) -> Result<Self, Error> {
        Self::parse(&read(path)?, path)
    }

    /// A recipe from its text, with `path` naming it in errors.
    pub fn parse(text: &str, path: &Path) -> Result<Self, Error> {
        // A recipe can call the kits' operations.
        kit::operations()?;
        ron_options().from_str(text).map_err(|source| Error::Ron {
            path: path.into(),
            source: Box::new(source),
        })
    }

    /// Stable topological order; declaration order breaks ties.
    pub fn ordered_steps(&self) -> Result<Vec<&Step>, Error> {
        let ids: BTreeSet<_> = self.steps.iter().map(|step| &step.id).collect();
        for step in &self.steps {
            for dependency in &step.after {
                if !ids.contains(dependency) {
                    return Err(Error::Invalid(format!(
                        "step {} references unknown dependency {}",
                        step.id, dependency
                    )));
                }
            }
        }
        let mut complete = BTreeSet::new();
        let mut ordered = Vec::new();
        while ordered.len() < self.steps.len() {
            let next = self.steps.iter().find(|step| {
                !complete.contains(&step.id) && step.after.iter().all(|id| complete.contains(id))
            });
            let Some(step) = next else {
                return Err(Error::Invalid("cyclic recipe dependencies".into()));
            };
            complete.insert(&step.id);
            ordered.push(step);
        }
        Ok(ordered)
    }
}
