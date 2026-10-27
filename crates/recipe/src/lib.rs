//! Versioned declarative patch recipes and device profiles.
//!
//! Where checks belong: loading a profile or recipe checks everything knowable
//! without firmware (schema, references between steps, which partitions a port can
//! have, well-formed operation parameters); planning adds asset presence and hashes
//! and the partitions the firmware supplies. Applying a recipe checks only what
//! depends on firmware content: conditions, `before` hashes, match counts and the
//! entries an operation expects.

mod desugar;
mod expr;
mod profile;
pub mod tree;
pub mod value;
pub mod visit;
mod xml;
pub use expr::Expr;
pub use profile::{
    Device, Port, Profile, ROM, STANDARD_SYSTEM, STOCK, Settings, SlotScheme, Stock, SuperLayout,
    VENDOR_SIDE, default_mount_point, system_mount_point,
};
pub use xml::{XmlEdit, XmlMatch, XmlName, XmlPath, XmlPredicate};

use model::{DevicePath, Name, RelativePath, Sha256Digest};
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
    #[error("invalid profile {path}: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("cannot encode recipe: {0}")]
    Encode(#[from] ron::Error),
    #[error("invalid configuration: {0}")]
    Invalid(String),
    #[error(transparent)]
    Model(#[from] model::Error),
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
    /// String expressions, evaluated against the tree composed so far and
    /// substituted for `{name}` in the step's condition and operations. One that has
    /// no value skips the step.
    #[serde(rename = "let", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bindings: BTreeMap<Name, Expr>,
    /// A boolean expression; the step applies only when it holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Expr>,
    pub operations: Vec<Operation>,
}

impl PropertyTarget {
    pub fn partition(&self) -> Option<&Name> {
        match self {
            Self::Partition(name) => Some(name),
            Self::File(_) => None,
        }
    }
}

/// The firmware an import reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Firmware {
    Rom,
    Stock,
}

impl Firmware {
    pub fn name(self) -> &'static str {
        match self {
            Self::Rom => ROM,
            Self::Stock => STOCK,
        }
    }
}

/// Every operation but `Import` changes the build: the partitions being produced.
/// Paths are device paths (`/vendor/etc/fstab.qcom`), resolved to a partition by its
/// mount point; `system` is mounted at `/` on system-as-root devices.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Operation {
    /// Copy what `pattern` (a device path, globs allowed) matches in a firmware into
    /// the build, at the same path or at `to`, overwriting what is there. An
    /// overwritten entry keeps the build's metadata; a new one has the firmware's,
    /// or with `metadata: Default` Android's defaults.
    Import {
        from: Firmware,
        pattern: DevicePath,
        /// Where it goes, if not to the same path. A glob keeps its path.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<DevicePath>,
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
    Relocate {
        from: DevicePath,
        to: DevicePath,
    },
    /// Write a recipe asset (a file, or a directory's files) at `to`, overwriting
    /// what is there. Overwritten files keep their metadata; new entries get
    /// Android's defaults.
    Install {
        asset: RelativePath,
        to: DevicePath,
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
    TextPatch {
        target: DevicePath,
        patch: RelativePath,
    },
    /// Overwrite bytes at offsets of a file whose content must be `before`.
    BinaryPatch {
        target: DevicePath,
        before: Sha256Digest,
        /// Offsets and the bytes written there, as hex: `{487572: "a9008052"}`.
        edits: BTreeMap<u64, Hex>,
    },
    Xml {
        target: DevicePath,
        /// Optional guard on the file's exact content before the edit.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before: Option<Sha256Digest>,
        edits: Vec<XmlEdit>,
    },
    /// Decode an APK or JAR, apply `edits` inside it and rebuild it. The edits are
    /// ordinary operations with paths from the package's root
    /// (`/res/values/strings.xml`), plus the smali edits only a package has.
    Package {
        target: DevicePath,
        /// Optional guard on the file's exact content before the edit.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before: Option<Sha256Digest>,
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
    Append {
        target: DevicePath,
        content: Data,
    },
}

/// What new entries an import brings get: the firmware's metadata, Android's
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

/// What `Append` adds: text, or a firmware file's content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Data {
    Text(String),
    File { from: Firmware, path: DevicePath },
}

/// The metadata `SetMetadata` sets; what it leaves unset stays.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overrides {
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub mode: Option<u16>,
    pub selinux: Option<String>,
}

/// Where a property edit or lookup applies: a partition's `.prop` files, or one file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum PropertyTarget {
    Partition(Name),
    File(DevicePath),
}

impl TryFrom<String> for PropertyTarget {
    type Error = model::Error;

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

/// Metadata for new files whose path (relative to the partition) matches `path`,
/// where `*` and `?` match within one component.
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
        let invalid = || format!("invalid hex bytes {text:?}");
        if text.is_empty() || !text.len().is_multiple_of(2) {
            return Err(invalid());
        }
        (0..text.len())
            .step_by(2)
            .map(|at| {
                u8::from_str_radix(text.get(at..at + 2).ok_or_else(invalid)?, 16)
                    .map_err(|_| invalid())
            })
            .collect::<Result<_, _>>()
            .map(Self)
    }
}

impl From<Hex> for String {
    fn from(hex: Hex) -> Self {
        hex.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

impl Operation {
    fn name(&self) -> &'static str {
        match self {
            Self::Import { .. } => "Import",
            Self::Remove { .. } => "Remove",
            Self::Relocate { .. } => "Relocate",
            Self::Install { .. } => "Install",
            Self::SetMetadata { .. } => "SetMetadata",
            Self::SetProperty { .. } => "SetProperty",
            Self::RemoveProperty { .. } => "RemoveProperty",
            Self::TextPatch { .. } => "TextPatch",
            Self::BinaryPatch { .. } => "BinaryPatch",
            Self::Xml { .. } => "Xml",
            Self::Package { .. } => "Package",
            Self::ReplaceMethod { .. } => "ReplaceMethod",
            Self::InsertMethod { .. } => "InsertMethod",
            Self::StubMethod { .. } => "StubMethod",
            Self::AssertHash { .. } => "AssertHash",
            Self::EnableFeature(..) => "EnableFeature",
            Self::DisableFeature(..) => "DisableFeature",
            Self::Append { .. } => "Append",
        }
    }

    /// The smali edits, which need a package's classes.
    fn package_only(&self) -> bool {
        matches!(
            self,
            Self::ReplaceMethod { .. } | Self::InsertMethod { .. } | Self::StubMethod { .. }
        )
    }

    /// What can run on a package's files: content edits, installs and removals by
    /// path, and the smali edits.
    fn in_package(&self) -> bool {
        match self {
            Self::Remove {
                from: None,
                partition: None,
                pattern,
            } => pattern.starts_with('/'),
            Self::TextPatch { .. }
            | Self::BinaryPatch { .. }
            | Self::Xml { .. }
            | Self::Install { .. }
            | Self::Append { .. }
            | Self::AssertHash { .. } => true,
            other => other.package_only(),
        }
    }

    /// Checks that can fail before any image is touched; the patch crate checks the rest on apply.
    fn validate(&self) -> Result<(), Error> {
        match self {
            Self::SetProperty { key, value, .. } if !property_key(key) || value.contains('\n') => {
                Err(Error::Invalid(format!("invalid property {key:?}")))
            }
            Self::RemoveProperty { key, .. } if !property_key(key) => {
                Err(Error::Invalid(format!("invalid property key {key:?}")))
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
            Self::SetMetadata {
                uid,
                gid,
                mode,
                selinux,
                ..
            }
            | Self::Import {
                metadata:
                    Metadata::Explicit {
                        uid,
                        gid,
                        mode,
                        selinux,
                    },
                ..
            } => {
                if uid.is_none() && gid.is_none() && mode.is_none() && selinux.is_none() {
                    return Err(Error::Invalid("metadata that sets nothing".into()));
                }
                if mode.is_some_and(|mode| mode > 0o7777) {
                    return Err(Error::Invalid(format!("invalid mode {mode:?}")));
                }
                match selinux {
                    Some(label)
                        if label.splitn(4, ':').filter(|part| !part.is_empty()).count() != 4
                            || label.contains(|c: char| c == '\0' || c.is_whitespace()) =>
                    {
                        Err(Error::Invalid(format!("invalid SELinux label {label:?}")))
                    }
                    _ => Ok(()),
                }
            }
            Self::BinaryPatch { edits, .. } => {
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
            Self::Xml { edits, .. } => xml::validate(edits),
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
        self.tree()?.walk(&mut visit::Check::default())?;
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

/// `value` with `{name}` replaced by each binding's value, round-tripped through RON
/// so the result is validated as if written that way. Bound values are restricted
/// to `[A-Za-z0-9._-]` because they come from firmware.
pub fn substitute<T: Serialize + serde::de::DeserializeOwned>(
    value: &T,
    bindings: &BTreeMap<Name, String>,
) -> Result<T, Error> {
    let mut text = ron_options().to_string(value)?;
    for (name, bound) in bindings {
        if bound.is_empty()
            || !bound
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err(Error::Invalid(format!(
                "bound value {bound:?} for {name} is not [A-Za-z0-9._-]"
            )));
        }
        text = text.replace(&format!("{{{name}}}"), bound);
    }
    ron_options()
        .from_str(&text)
        .map_err(|source| Error::Invalid(format!("substituted step does not parse: {source}")))
}
