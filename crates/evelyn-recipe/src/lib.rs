//! The recipe language: versioned declarative patch recipes, desugared into a
//! primitive tree whose types and partitions are checked before any firmware is read.
//!
//! Where checks belong: loading a profile or recipe checks everything knowable
//! without firmware (schema, references between steps, which partitions a port can
//! have, well-formed operation parameters); planning adds asset presence and hashes
//! and the partitions the firmware supplies. Applying a recipe checks only what
//! depends on firmware content: conditions, hashes, match counts and the
//! entries an operation expects.

mod desugar;
pub mod tree;
pub mod value;
pub mod visit;
mod xml;
pub use xml::{XmlEdit, XmlMatch, XmlName, XmlPath, XmlPredicate, XmlStep, escape_attribute};

use android_firmware::{DevicePath, Firmware, Name, RelativePath, Sha256Digest};
use serde::{Deserialize, Serialize};
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
    #[error("cannot encode recipe: {0}")]
    Encode(#[from] ron::Error),
    #[error("invalid configuration: {0}")]
    Invalid(String),
    #[error(transparent)]
    Firmware(#[from] android_firmware::Error),
}

fn read(path: &Path) -> Result<String, Error> {
    fs::read_to_string(path).map_err(|source| Error::Read {
        path: path.into(),
        source,
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub schema: u32,
    pub name: Name,
    pub steps: Vec<Step>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: Name,
    #[serde(default)]
    pub after: Vec<Name>,
    /// Names bound to string expressions, evaluated against the tree composed so far,
    /// for the step's condition and operations to use as `Var("name")`. One that has
    /// no value skips the step.
    #[serde(rename = "let", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bindings: BTreeMap<Name, tree::Expr>,
    /// A boolean expression; the step applies only when it holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<tree::Expr>,
    pub operations: Vec<Operation>,
}

/// Operations change the build: the partitions being produced.
/// Paths are device paths (`/vendor/etc/fstab.qcom`), resolved to a partition by its
/// mount point; `system` is mounted at `/` on system-as-root devices.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Operation {
    /// Copy what `path` matches into the build, at the same path or at `to`,
    /// overwriting what is there: in a firmware a device path, globs allowed; in the
    /// recipe's assets a path relative to its directory, a file or a directory's
    /// files, which needs `to`. An overwritten entry keeps the build's metadata; a
    /// new one has the firmware's, assets having none Android's defaults, or with
    /// `metadata: Default` Android's defaults. What `path` names must be there.
    Copy {
        from: tree::Source,
        /// A string expression, like `to`.
        path: tree::Expr,
        /// Where it goes, if not to the same path. A glob keeps its path.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<tree::Expr>,
        #[serde(default, skip_serializing_if = "Metadata::is_copy")]
        metadata: Metadata,
    },
    /// Remove what `pattern` matches from the build. A relative pattern is tried in
    /// the directory of every build partition the scope selects: those from the ROM
    /// or the stock firmware (`from`), named by `partition` (a name or glob), both,
    /// or with neither, all of them. A `/...` pattern is a device path, outside any
    /// scope. A pattern that matches nothing is reported, not an error; a scope that
    /// selects no partition is an error.
    Remove {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<Firmware>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        partition: Option<String>,
        pattern: String,
    },
    /// Set metadata Android's defaults do not give, on every entry `pattern` (a
    /// device path, globs allowed) matches in the build.
    SetMetadata {
        pattern: DevicePath,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        uid: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gid: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selinux: Option<String>,
    },
    /// Set a property everywhere it is defined, else add it: without a target in
    /// every partition, else in product's `build.prop`; with a partition,
    /// in every file in it defining the key, else its `build.prop`; with a file,
    /// in that file.
    SetProperty {
        key: String,
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<PropertyTarget>,
    },
    /// Remove every definition in the same scope as `SetProperty`; one must exist.
    RemoveProperty {
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<PropertyTarget>,
    },
    /// Apply a unified diff (an asset) to a file and its hardlinks.
    Diff {
        target: DevicePath,
        patch: RelativePath,
    },
    /// Overwrite bytes at offsets of a file. What is there decides what the bytes
    /// mean, so an `AssertHash` of the file should come first.
    WriteBytes {
        target: DevicePath,
        /// Offsets and the bytes written there, as hex: `{487572: "a9008052"}`.
        edits: BTreeMap<u64, Hex>,
    },
    EditXml {
        target: DevicePath,
        edits: Vec<XmlEdit>,
    },
    /// Decode an APK or JAR, apply `edits` inside it and rebuild it. The edits are
    /// ordinary operations with paths from the package's root
    /// (`/res/values/strings.xml`), plus the smali edits only a package has.
    Package {
        target: DevicePath,
        edits: Vec<Operation>,
    },
    /// Inside a `Package`: replace a smali method with the asset `body`.
    ReplaceMethod {
        class: String,
        method: String,
        body: RelativePath,
    },
    /// Inside a `Package`: add the asset `body` as a new smali method.
    InsertMethod {
        class: String,
        method: String,
        body: RelativePath,
    },
    /// Inside a `Package`: replace a method's body so it only returns `returns`.
    StubMethod {
        class: String,
        method: String,
        returns: Returns,
    },
    /// The file's content must have this SHA-256.
    AssertHash {
        target: DevicePath,
        sha256: Sha256Digest,
    },
    /// Declare a feature, in its kind's file: added there, or replaced if it is.
    EnableFeature(Feature),
    /// Remove a feature (`feature`, `oplus-feature` or `app_feature`) from every
    /// permissions and extension XML in the build.
    DisableFeature(String),
    /// Add `content` at the end of a file, on a line of its own.
    Append { target: DevicePath, content: Data },
    /// Patch one of the device's CIL policy files with TE `policies` (assets), written
    /// with Android's policy macros. The result must resolve with the device's other
    /// policy files, as init loads them.
    AddPolicy {
        target: DevicePath,
        policies: Vec<RelativePath>,
    },
}

/// What new entries a copy brings get: the firmware's metadata, Android's
/// defaults for where they are on the device, or those defaults with what
/// `Explicit` names set instead.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Metadata {
    #[default]
    Copy,
    Default,
    Explicit {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        uid: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gid: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selinux: Option<String>,
    },
}

impl Metadata {
    fn is_copy(&self) -> bool {
        *self == Self::Copy
    }
}

/// A feature `EnableFeature` declares, by the element it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Feature {
    /// `<feature name=…>`, in `/my_product/etc/permissions/com.oplus.android-features.xml`.
    Android(String),
    /// `<oplus-feature name=…>`, in `/my_product/etc/permissions/oplus.feature.android.xml`.
    Oplus(String),
    /// `<app_feature name=… args=…>`, in `file`.
    App {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args: Option<String>,
        file: DevicePath,
    },
}

/// What `Append` adds: text, a firmware file's content, or the lines of one that
/// contain any of `containing` and none of `except`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Data {
    Text(String),
    File {
        from: Firmware,
        path: DevicePath,
    },
    Lines {
        from: Firmware,
        path: DevicePath,
        containing: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        except: Vec<String>,
    },
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
    fn validate(&self) -> Result<(), Error> {
        if *self == Self::default() {
            return Err(Error::Invalid("metadata that sets nothing".into()));
        }
        if self.mode.is_some_and(|mode| mode > 0o7777) {
            return Err(Error::Invalid(format!("invalid mode {:?}", self.mode)));
        }
        match &self.selinux {
            Some(label)
                if label.splitn(4, ':').filter(|part| !part.is_empty()).count() != 4
                    || label.contains(|c: char| c == '\0' || c.is_whitespace()) =>
            {
                Err(Error::Invalid(format!("invalid SELinux label {label:?}")))
            }
            _ => Ok(()),
        }
    }
}

/// Where a property edit or lookup applies: a partition's `.prop` files, or one file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum PropertyTarget {
    Partition(Name),
    File(DevicePath),
}

impl PropertyTarget {
    pub fn partition(&self) -> Option<&Name> {
        match self {
            Self::Partition(name) => Some(name),
            Self::File(_) => None,
        }
    }
}

impl TryFrom<String> for PropertyTarget {
    type Error = android_firmware::Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.starts_with('/') {
            Ok(Self::File(value.try_into()?))
        } else {
            Ok(Self::Partition(value.try_into()?))
        }
    }
}

impl From<PropertyTarget> for String {
    fn from(value: PropertyTarget) -> Self {
        match value {
            PropertyTarget::Partition(name) => name.into(),
            PropertyTarget::File(path) => path.into(),
        }
    }
}

/// What a stubbed method returns: nothing (`V`), zero/false/null, or one/true.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum Returns {
    Void,
    Zero,
    One,
}

/// Bytes written as hex, two digits each: `"a9008052"`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Hex(pub Vec<u8>);

impl TryFrom<String> for Hex {
    type Error = String;

    fn try_from(text: String) -> Result<Self, String> {
        android_firmware::unhex(&text)
            .filter(|bytes| !bytes.is_empty())
            .map(Self)
            .ok_or_else(|| format!("invalid hex bytes {text:?}"))
    }
}

impl From<Hex> for String {
    fn from(hex: Hex) -> Self {
        android_firmware::hex(&hex.0)
    }
}

impl Operation {
    fn name(&self) -> &'static str {
        match self {
            Self::Copy { .. } => "Copy",
            Self::Remove { .. } => "Remove",
            Self::SetMetadata { .. } => "SetMetadata",
            Self::SetProperty { .. } => "SetProperty",
            Self::RemoveProperty { .. } => "RemoveProperty",
            Self::Diff { .. } => "Diff",
            Self::WriteBytes { .. } => "WriteBytes",
            Self::EditXml { .. } => "EditXml",
            Self::Package { .. } => "Package",
            Self::ReplaceMethod { .. } => "ReplaceMethod",
            Self::InsertMethod { .. } => "InsertMethod",
            Self::StubMethod { .. } => "StubMethod",
            Self::AssertHash { .. } => "AssertHash",
            Self::EnableFeature(..) => "EnableFeature",
            Self::DisableFeature(..) => "DisableFeature",
            Self::Append { .. } => "Append",
            Self::AddPolicy { .. } => "AddPolicy",
        }
    }

    /// The metadata a `SetMetadata`, or a copy's `Metadata::Explicit`, sets.
    fn overrides(&self) -> Option<Overrides> {
        let (Self::SetMetadata {
            uid,
            gid,
            mode,
            selinux,
            ..
        }
        | Self::Copy {
            metadata:
                Metadata::Explicit {
                    uid,
                    gid,
                    mode,
                    selinux,
                },
            ..
        }) = self
        else {
            return None;
        };
        Some(Overrides {
            uid: *uid,
            gid: *gid,
            mode: *mode,
            selinux: selinux.clone(),
        })
    }

    /// The smali edits, which need a package's classes.
    fn package_only(&self) -> bool {
        matches!(
            self,
            Self::ReplaceMethod { .. } | Self::InsertMethod { .. } | Self::StubMethod { .. }
        )
    }

    /// What can run on a package's files: content edits, copies of assets and
    /// removals by path, and the smali edits.
    fn in_package(&self) -> bool {
        match self {
            Self::Remove {
                from: None,
                partition: None,
                pattern,
            } => pattern.starts_with('/'),
            Self::Diff { .. }
            | Self::WriteBytes { .. }
            | Self::EditXml { .. }
            | Self::Copy {
                from: tree::Source::Assets,
                ..
            }
            | Self::Append { .. }
            | Self::AssertHash { .. } => true,
            other => other.package_only(),
        }
    }

    /// Checks that can fail before any image is touched; the patch crate checks the rest on apply.
    fn validate(&self) -> Result<(), Error> {
        if let Some(overrides) = self.overrides() {
            return overrides.validate();
        }
        match self {
            Self::SetProperty { key, value, .. } if !property_key(key) || value.contains('\n') => {
                Err(Error::Invalid(format!("invalid property {key:?}")))
            }
            Self::RemoveProperty { key, .. } if !property_key(key) => {
                Err(Error::Invalid(format!("invalid property key {key:?}")))
            }
            Self::AddPolicy { policies, .. } if policies.is_empty() => {
                Err(Error::Invalid("AddPolicy needs at least one policy".into()))
            }
            Self::Append {
                content: Data::Lines { containing, .. },
                ..
            } if containing.is_empty() => Err(Error::Invalid(
                "Lines needs at least one string a line contains".into(),
            )),
            // Paths written out are checked now, computed ones when they are used.
            Self::Copy { from, path, to, .. } => {
                if *from == tree::Source::Assets && to.is_none() {
                    return Err(Error::Invalid("copying an asset needs a to:".into()));
                }
                if let Some(path) = path.literal_str() {
                    match from {
                        tree::Source::Assets => {
                            RelativePath::try_from(path.to_owned()).map(drop)?
                        }
                        _ => DevicePath::try_from(path.to_owned()).map(drop)?,
                    }
                }
                if let Some(to) = to.as_ref().and_then(tree::Expr::literal_str) {
                    DevicePath::try_from(to.to_owned())?;
                }
                Ok(())
            }
            Self::Remove {
                partition, pattern, ..
            } => {
                if let Some(partition) = partition
                    && (partition.is_empty()
                        || !partition
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"_-*?".contains(&b)))
                {
                    return Err(Error::Invalid(format!("invalid partition {partition:?}")));
                }
                if pattern.starts_with('/') {
                    DevicePath::try_from(pattern.clone())?;
                } else {
                    RelativePath::try_from(pattern.clone())?;
                }
                Ok(())
            }
            Self::StubMethod { class, .. }
            | Self::ReplaceMethod { class, .. }
            | Self::InsertMethod { class, .. }
                if !(class.starts_with('L') && class.ends_with(';') && class.len() > 2) =>
            {
                Err(Error::Invalid(format!(
                    "class {class:?} is not written as Lcom/example/Name;"
                )))
            }
            Self::WriteBytes { edits, .. } => {
                let mut end = 0;
                for (offset, bytes) in edits {
                    if *offset < end {
                        return Err(Error::Invalid(format!(
                            "binary edit at {offset} overlaps the one before it"
                        )));
                    }
                    end = offset + bytes.0.len() as u64;
                }
                Ok(())
            }
            Self::EditXml { edits, .. } => xml::validate(edits),
            Self::Package { edits, .. } => edits.iter().try_for_each(|edit| {
                if !edit.in_package() {
                    return Err(Error::Invalid(format!(
                        "{} does not work inside a Package",
                        edit.name()
                    )));
                }
                edit.validate()
            }),
            _ => Ok(()),
        }
    }
}

/// Optional fields are written as plain values, without `Some(...)`.
fn ron_options() -> ron::Options {
    ron::Options::default().with_default_extension(ron::extensions::Extensions::IMPLICIT_SOME)
}

fn property_key(key: &str) -> bool {
    !key.is_empty() && !key.contains(['=', '\n']) && !key.contains(char::is_whitespace)
}

impl Recipe {
    pub fn write(&self, writer: impl io::Write) -> Result<(), Error> {
        self.validate()?;
        ron_options().to_io_writer_pretty(writer, self, ron::ser::PrettyConfig::default())?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self, Error> {
        let recipe: Self = ron_options()
            .from_str(&read(path)?)
            .map_err(|source| Error::Ron {
                path: path.into(),
                source: Box::new(source),
            })?;
        recipe.validate()?;
        Ok(recipe)
    }

    fn validate(&self) -> Result<(), Error> {
        if self.schema != 1 {
            return Err(Error::Invalid(format!(
                "unsupported recipe schema {}",
                self.schema
            )));
        }
        let mut ids = BTreeSet::new();
        for step in &self.steps {
            if !ids.insert(&step.id) {
                return Err(Error::Invalid(format!("duplicate step {}", step.id)));
            }
            for operation in &step.operations {
                if operation.package_only() {
                    return Err(Error::Invalid(format!(
                        "{} works only inside a Package",
                        operation.name()
                    )));
                }
                operation.validate()?;
            }
        }
        self.tree()?.walk(&mut visit::Check)?;
        Ok(())
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
