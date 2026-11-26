//! The recipe language: steps of operations, applied in the order they are
//! written, whose strings can name the values the operations before them bind. A recipe is evaluated as it is
//! written; what is wrong with one is found when it is applied.

mod expr;
pub mod kit;
mod location;
mod xml;
pub use expr::{Expr, substitute, written_out};
pub use location::Location;
pub use xml::{
    SelectorError, XmlEdit, XmlMatch, XmlName, XmlOp, XmlPredicate, XmlStep, escape_attribute,
};

use android::hash::Sha256Digest;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("cannot read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid recipe {path}")]
    Ron {
        path: PathBuf,
        #[source]
        source: Box<ron::error::SpannedError>,
    },
    #[error(transparent)]
    Selector(#[from] SelectorError),
    #[error("{name} is bound to {value:?}, which is not [A-Za-z0-9._-]")]
    Token { name: String, value: String },
    #[error("{{{0}}} is not bound")]
    Unbound(String),
    #[error("{kit} names {file}, which is not a device path to a property file")]
    KitPropertyFile { kit: String, file: String },
    #[error("{kit} defines {name}, which is already an operation")]
    KitOperation { kit: String, name: String },
    #[error("the kits are loaded once, before any recipe is parsed")]
    KitsLoaded,
}

fn read(path: &Path) -> Result<String, Error> {
    fs::read_to_string(path).map_err(|source| Error::Read {
        path: path.into(),
        source,
    })
}

/// A recipe: its steps, applied in the order they are written.
#[derive(Clone, Debug, Deserialize)]
#[serde(transparent)]
pub struct Recipe {
    pub steps: Vec<Step>,
}

/// A step: its name, which reports and errors give, and its operations, applied
/// in order: `Step("vndk-apex", [ … ])`.
#[derive(Clone, Debug, Deserialize)]
pub struct Step(pub String, pub Vec<Operation>);

impl Step {
    pub fn id(&self) -> &str {
        &self.0
    }

    pub fn operations(&self) -> &[Operation] {
        &self.1
    }
}

/// Operations change what they apply to: the build, the partitions being
/// produced, or inside a `Package`, the package. Paths are device paths
/// (`/vendor/etc/fstab.qcom`), resolved to a partition by its mount point;
/// `system` is mounted at `/` on system-as-root devices. Inside a `Package` they
/// are paths from its root (`/res/values/strings.xml`), and inside a `BootImage`,
/// paths in its ramdisk (`/first_stage_ramdisk/fstab.qcom`).
///
/// Some operations open a file as a document of a kind (`Xml`, `Text`, `Bytes`,
/// `Props`, `Policy`) and apply that kind's edits to it, in order.
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
    /// Write a recipe asset, a file or a directory's files, at a path, with
    /// Android's defaults: `Install("odm/bin/foo", "/odm/bin/foo")` is `Copy` with
    /// `metadata: Default`, `overwrite` and `create_parent`.
    Install(String, Expr),
    /// Remove what a path matches, globs allowed. One that matches nothing is
    /// reported, not an error.
    Remove(String),
    /// Set metadata Android's defaults do not give, on every entry `pattern` (a
    /// path, globs allowed) matches.
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
    /// Edit properties: `All` of the build's property files, as init loads them,
    /// a `Partition("…")`'s, or one `Build("/…")` file.
    Props(PropScope, Vec<PropEdit>),
    /// Edit a file as text.
    Text(String, Vec<TextEdit>),
    /// Overwrite bytes at offsets of a file, written as hex: `{487572: "a9008052"}`.
    /// What is there decides what the bytes mean, so an `AssertHash` of the file
    /// should come first.
    Bytes(String, BTreeMap<u64, Hex>),
    /// Edit an XML file, in order: set an element's text or an attribute, remove
    /// elements, or put a child element under a parent.
    Xml(String, Vec<XmlEdit>),
    /// Patch one of the device's CIL policy files.
    Policy(String, Vec<PolicyEdit>),
    /// Decode an APK or JAR and apply operations inside it. It is rebuilt once,
    /// when the build is written, and signed only if a `Sign` says so. Inside it,
    /// paths are from its root, and it has the smali edits and `Resources`.
    Package(String, Vec<Operation>),
    /// Apply operations to a stock boot image, `boot`, `init_boot` or
    /// `vendor_boot`, which is built again if they change it. Inside it, paths
    /// are paths in its ramdisk, as the device's rootfs has them; `Stock("/…")`
    /// is a path in the stock image's ramdisk. Only the file operations work
    /// inside it.
    BootImage(String, Vec<Operation>),
    /// Inside a `BootImage` of a v4 `vendor_boot`: edit its bootconfig, whose
    /// `androidboot.*` keys init sets as `ro.boot.*` properties.
    Bootconfig(Vec<PropEdit>),
    /// Sign an APK with Evelyn's key when the build is written, as Android needs
    /// an APK it verifies to be once its contents change.
    Sign(String),
    /// Inside a `Package`: set the package's values resources.
    Resources(Vec<ResourceEdit>),
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
    /// Bind a name to a string read from what the operations apply to, for the
    /// strings of the operations after it to name as `{name}`. One that has no
    /// value skips them, and is reported. The value is a token,
    /// `[A-Za-z0-9._-]`: it comes from firmware and goes into paths.
    Let(String, Expr),
    /// Apply operations if the condition holds. One whose condition does not hold
    /// is not reported.
    When(Expr, Vec<Operation>),
    /// Apply operations for each string of a list, or each entry of a selection,
    /// with `{name}` bound to it: the string, or the entry's device path. One over
    /// nothing is reported; what its operations report is reported once, if they
    /// report it for every one.
    ForEach(Expr, String, Vec<Operation>),
    /// The condition must hold, or the build fails with the message.
    Assert(Expr, String),
    /// The file's content must have this SHA-256.
    AssertHash(String, Sha256Digest),
    /// An operation of a kit, by the name the kit gives it, with the strings its
    /// parameters are given.
    #[serde(skip)]
    Call {
        name: String,
        args: BTreeMap<String, String>,
    },
}

/// Which property files `Props` edits, and a `Property` read reads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PropScope {
    /// Every property file of the build, as init loads them: an edit changes each
    /// file that defines the key, else adds it to the one init loads last, so the
    /// value takes effect; a read gets the value the device ends up with.
    All,
    /// The files of a partition init loads: an edit changes each that defines the
    /// key, else adds it to the partition's own `build.prop`.
    Partition(String),
    /// One file, by its device path.
    Build(String),
}

impl PropScope {
    /// The files it names, as the property operations take them: none for
    /// `All`.
    pub fn location(&self) -> Option<Location> {
        match self {
            Self::All => None,
            Self::Partition(name) => Some(Location::Partition(name.clone())),
            Self::Build(path) => Some(Location::Build(path.clone())),
        }
    }
}

/// An edit of properties.
#[derive(Clone, Debug, Deserialize)]
pub enum PropEdit {
    /// Set a key to a value everywhere it is defined, else add it.
    Set(String, String),
    /// Remove every definition of a key. Where none is, that is reported, not an
    /// error.
    Remove(String),
}

/// An edit of a file as text.
#[derive(Clone, Debug, Deserialize)]
pub enum TextEdit {
    /// Add a string expression at the end, on a line of its own.
    Append(Expr),
    /// Apply a unified diff, an asset, to the file and its hardlinks.
    Diff(String),
}

/// An edit of a CIL policy file. The result must resolve with the device's
/// other policy files, as init loads them.
#[derive(Clone, Debug, Deserialize)]
pub enum PolicyEdit {
    /// Add a TE policy, an asset, written with Android's policy macros. Policies
    /// added one after another are added together, so one can use what another
    /// declares.
    Add(String),
    /// Add what the compiled policy `from` (the ROM's precompiled odm policy, by
    /// default) has for the types and attributes `types` names, globs allowed:
    /// their declarations, and the rules that name them. What the device's policy
    /// already declares is not declared again, and a rule that needs a name
    /// nothing defines is left out, unless a later import defines it.
    Import {
        types: Vec<String>,
        #[serde(default = "precompiled_policy")]
        from: Location,
    },
    /// Remove the types and attributes `types` names, globs allowed, from the
    /// file: their declarations, and every statement that names them, except an
    /// attribute set, which only loses them as members. The names are matched
    /// against what the device's policy files declare, so an attribute another
    /// file declares is removed from this one's rules too.
    Remove { types: Vec<String> },
}

/// An edit of a package's values resources.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ResourceEdit {
    /// Set a resource of `type` (`string`, `bool`, `integer`, `color`, `dimen`,
    /// `fraction`, `string-array`, `integer-array`) and `name` in a configuration
    /// (`""` for the default `values/`, or a qualifier like `"en-rIN"`), which
    /// must have it, or with `create`, gets it if it does not. A value is what
    /// goes between the tags, an array's a list of its items.
    Set {
        #[serde(rename = "type")]
        kind: String,
        name: String,
        config: String,
        value: ResourceValue,
        #[serde(default)]
        create: bool,
    },
}

/// A resource's value: one, or an array's items.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum ResourceValue {
    One(String),
    Items(Vec<String>),
}

/// Where `Import` takes a policy from unless it is told: the ROM's
/// precompiled odm policy, which has its vendor policy compiled in.
fn precompiled_policy() -> Location {
    Location::Rom("/odm/etc/selinux/precompiled_sepolicy".to_owned())
}

/// The built-in operations' names.
pub(crate) const BUILT_IN_OPERATIONS: [&str; 24] = [
    "Copy",
    "Import",
    "Install",
    "Remove",
    "SetMetadata",
    "Props",
    "Text",
    "Bytes",
    "Xml",
    "Policy",
    "Package",
    "BootImage",
    "Bootconfig",
    "Sign",
    "Resources",
    "ReplaceMethod",
    "InsertMethod",
    "StubMethod",
    "ReplaceOperand",
    "Let",
    "When",
    "ForEach",
    "Assert",
    "AssertHash",
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
            Self::Install(..) => "Install",
            Self::Remove(..) => "Remove",
            Self::SetMetadata { .. } => "SetMetadata",
            Self::Props(..) => "Props",
            Self::Text(..) => "Text",
            Self::Bytes(..) => "Bytes",
            Self::Xml(..) => "Xml",
            Self::Policy(..) => "Policy",
            Self::Package(..) => "Package",
            Self::BootImage(..) => "BootImage",
            Self::Bootconfig(..) => "Bootconfig",
            Self::Sign(..) => "Sign",
            Self::Resources(..) => "Resources",
            Self::ReplaceMethod { .. } => "ReplaceMethod",
            Self::InsertMethod { .. } => "InsertMethod",
            Self::StubMethod { .. } => "StubMethod",
            Self::ReplaceOperand { .. } => "ReplaceOperand",
            Self::Let(..) => "Let",
            Self::When(..) => "When",
            Self::ForEach(..) => "ForEach",
            Self::Assert(..) => "Assert",
            Self::AssertHash(..) => "AssertHash",
            Self::Call { name, .. } => name.as_str(),
        }
    }

    /// What can run on a package's files: file and content edits, copies of
    /// assets, the smali edits and `Resources`.
    pub fn in_package(&self) -> bool {
        matches!(
            self,
            Self::Remove(..)
                | Self::Text(..)
                | Self::Bytes(..)
                | Self::Xml(..)
                | Self::Copy { .. }
                | Self::Install(..)
                | Self::Resources(..)
                | Self::Assert(..)
                | Self::AssertHash(..)
                | Self::ReplaceMethod { .. }
                | Self::InsertMethod { .. }
                | Self::StubMethod { .. }
                | Self::ReplaceOperand { .. }
                | Self::Let(..)
                | Self::ForEach(..)
                | Self::When(..)
        )
    }

    /// What can run on a boot image: file and content edits and copies in its
    /// ramdisk, and `Bootconfig`.
    pub fn in_boot_image(&self) -> bool {
        matches!(
            self,
            Self::Bootconfig(..)
                | Self::Copy { .. }
                | Self::Import(..)
                | Self::Install(..)
                | Self::Remove(..)
                | Self::SetMetadata { .. }
                | Self::Text(..)
                | Self::Bytes(..)
                | Self::Xml(..)
                | Self::Assert(..)
                | Self::AssertHash(..)
                | Self::Let(..)
                | Self::ForEach(..)
                | Self::When(..)
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
            Self::Install(_, to) => found.extend(to.locations()),
            Self::Text(_, edits) => {
                for edit in edits {
                    if let TextEdit::Append(content) = edit {
                        found.extend(content.locations());
                    }
                }
            }
            Self::Policy(_, edits) => {
                for edit in edits {
                    if let PolicyEdit::Import { from, .. } = edit {
                        found.push(from);
                    }
                }
            }
            Self::Let(_, value) => found.extend(value.locations()),
            Self::Assert(condition, _) => found.extend(condition.locations()),
            Self::Package(_, operations) => {
                found.extend(operations.iter().flat_map(Self::locations))
            }
            // What it reads is in the boot image, which the build reads.
            Self::BootImage(..) => {}
            Self::ForEach(over, _, operations) => {
                found.extend(over.locations());
                found.extend(operations.iter().flat_map(Self::locations));
            }
            Self::When(condition, operations) => {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The recipes and kits Evelyn ships parse.
    #[test]
    fn built_ins_parse() {
        kit::operations().unwrap();
        for (name, text) in [
            ("vndk.ron", include_str!("../../../assets/recipes/vndk.ron")),
            (
                "debug.ron",
                include_str!("../../../assets/recipes/debug.ron"),
            ),
            (
                "selinux-permissive.ron",
                include_str!("../../../assets/recipes/selinux-permissive.ron"),
            ),
        ] {
            let recipe = Recipe::parse(text, Path::new(name)).unwrap();
            assert!(!recipe.steps.is_empty(), "{name}");
        }
    }

    /// The forms a recipe is written in parse as what they say.
    #[test]
    fn forms_parse() {
        let recipe = Recipe::parse(
            r#"[
                Step("one", [
                    Let("type", Property(Build("/my_product/build.prop"), "ro.type")),
                    Props(All, [Set("ro.a", "1"), Remove("ro.b")]),
                    Text("/vendor/etc/x", [Append(r"a\.b"), Diff("x.diff")]),
                    Policy("/vendor/etc/selinux/vendor_sepolicy.cil", [Add("a.te"), Import(types: ["t"]), Remove(types: ["t_*"])]),
                    ForEach(Partitions(Rom), "p", [Remove("{p}/app/X")]),
                    Package("/product/app/A.apk", [
                        Resources([Set(type: "string", name: "n", config: "", value: "v")]),
                        Resources([Set(type: "integer-array", name: "m", config: "en-rIN", value: ["1", "2"], create: true)]),
                    ]),
                    BootImage("vendor_boot", [
                        Text("/first_stage_ramdisk/fstab.qcom", [Append(Read(Asset("fstab")))]),
                        Bootconfig([Set("androidboot.a", "1"), Remove("androidboot.b")]),
                    ]),
                ]),
            ]"#,
            Path::new("test.ron"),
        )
        .unwrap();
        let [step] = &recipe.steps[..] else {
            panic!("one step")
        };
        assert_eq!(step.id(), "one");
        assert_eq!(step.operations().len(), 7);
    }
}
