//! SELinux CIL policy files open as documents, patched with TE rules through
//! libsepol.
//!
//! A policy file is parsed once, when a step first patches it, and is open in its
//! target from then on, holding the file's content. Each patch is added to it as
//! an input of its own, named for what it is: a TE asset by its path, an import by
//! what it takes and from where. Once every step is applied, the document is
//! compiled with the device's other policy files, as init compiles it at boot: an
//! error names the input, and so the patch, it is in.

use crate::patch::Context;
use crate::patch::target::IoAt;
use crate::patch::value::Located;
use android::path::RelativePath;
use sepolicy::CilPolicy;
use sepolicy::parser::ast::Policy;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use thiserror::Error;

/// Android's M4 macros, which TE policies are written with, in the order they
/// are loaded. Evelyn downloads them.
const MACROS: [&str; 2] = ["selinux/te_macros", "selinux/global_macros"];

/// Android's M4 macros, downloaded if they are not yet.
fn macros(context: &Context<'_>) -> Result<Vec<String>, crate::patch::Error> {
    MACROS
        .iter()
        .map(|name| {
            let path = context.tools.file(name)?;
            String::from_utf8(fs::read(&path).at(&path)?)
                .map_err(|_| crate::patch::Error::Unsupported("policies are UTF-8 text"))
        })
        .collect()
}

/// The TE `policies` (the recipe's assets), each by its path, with Android's M4
/// macros expanded.
pub(crate) fn assets(
    policies: &[RelativePath],
    context: &Context<'_>,
) -> Result<Vec<(String, Policy)>, crate::patch::Error> {
    let macros = macros(context)?;
    let macros: Vec<&str> = macros.iter().map(String::as_str).collect();
    policies
        .iter()
        .map(|path| {
            let file = context.asset(path)?;
            let name = file.display().to_string();
            let text = String::from_utf8(fs::read(&file).at(&file)?)
                .map_err(|_| crate::patch::Error::Unsupported("policies are UTF-8 text"))?;
            let expanded = sepolicy::parser::expand_with(&text, &macros).map_err(at(&name))?;
            Ok((name.clone(), parse(&name, &expanded)?))
        })
        .collect()
}

/// The TE policy `text`, named `name` in errors. Rules taken from a compiled
/// policy have no macros, so they are parsed as they are: a type named like a
/// macro stays a type.
pub(crate) fn parse(name: &str, text: &str) -> Result<Policy, Error> {
    sepolicy::parser::parse(text)
        .map_err(sepolicy::Error::from)
        .map_err(at(name))
}

/// An error in the policy named `name`.
fn at(name: &str) -> impl FnOnce(sepolicy::Error) -> Error {
    let name = name.to_owned();
    move |source| Error::Policy {
        name,
        source: Box::new(source),
    }
}

/// libsepol's errors are boxed: they are large, and would make every result as large.
#[derive(Debug, Error)]
pub enum Error {
    #[error("SELinux policy {name}")]
    Policy {
        name: String,
        #[source]
        source: Box<sepolicy::Error>,
    },
    #[error("the patched SELinux policy does not compile with the device's other policy files")]
    Uncompiled(#[source] Box<sepolicy::Error>),
    #[error("cannot render the patched SELinux policy")]
    Render(#[source] Box<sepolicy::Error>),
}

impl Error {
    /// The input of the policy that libsepol could not compile a statement of.
    pub(crate) fn input(&self) -> Option<&str> {
        match self {
            Error::Uncompiled(source) => match source.as_ref() {
                sepolicy::Error::CilCompile(error) => error.location()?.input.as_deref(),
                _ => None,
            },
            _ => None,
        }
    }
}

/// A CIL policy file open for patching.
pub(crate) struct Document {
    /// The file's device path, which its own text is loaded under.
    name: String,
    /// The file's text when it was opened.
    text: String,
    /// What steps added since, each under the name of its input.
    patches: Vec<(String, Policy)>,
    /// The file's text and every patch, which the file is rendered from.
    policy: CilPolicy,
    /// The types, attributes and aliases this file and the device's other
    /// policy files declare.
    declared: HashSet<String>,
    /// The compiled policies in a firmware, which does not change, that this file
    /// imports from: a recipe imports from the same one again and again.
    donors: BTreeMap<Located, sepolicy::SePolicy>,
}

impl Document {
    /// Open the policy file at `name`, `text`, with `others`, the device's other
    /// policy files (path, text).
    pub(crate) fn open(
        name: String,
        text: String,
        others: &[(String, String)],
    ) -> Result<Self, Error> {
        let policy = CilPolicy::new()
            .add_file_named(&name, &text)
            .map_err(at(&name))?;
        let mut declared: HashSet<String> = policy
            .declared_types()
            .map_err(at(&name))?
            .into_iter()
            .collect();
        for (path, other) in others {
            declared.extend(sepolicy::cil::declared_types(other).map_err(at(path))?);
        }
        Ok(Self {
            name,
            text,
            patches: Vec::new(),
            policy,
            declared,
            donors: BTreeMap::new(),
        })
    }

    /// The compiled policy in a firmware at `source` this file imported from.
    pub(crate) fn donor(&self, source: &Located) -> Option<&sepolicy::SePolicy> {
        self.donors.get(source)
    }

    /// Keep `donor`, the compiled policy in a firmware at `source`, for this
    /// file's next imports from it.
    pub(crate) fn keep_donor(&mut self, source: Located, donor: sepolicy::SePolicy) {
        self.donors.insert(source, donor);
    }

    /// What this file and the device's other policy files declare.
    pub(crate) fn declared(&self) -> &HashSet<String> {
        &self.declared
    }

    /// Add `rules` as an input named `name`, made unique if a patch already has it.
    pub(crate) fn add(&mut self, name: &str, rules: Policy) -> Result<(), Error> {
        let mut unique = name.to_owned();
        for n in 2.. {
            if !self.patches.iter().any(|(taken, _)| *taken == unique) {
                break;
            }
            unique = format!("{name} ({n})");
        }
        self.policy = self
            .policy
            .clone()
            .add_policy_named(&unique, &rules)
            .map_err(at(&unique))?;
        let added = CilPolicy::new()
            .add_policy(&rules)
            .and_then(|added| added.declared_types())
            .map_err(at(&unique))?;
        self.declared.extend(added);
        self.patches.push((unique, rules));
        Ok(())
    }

    /// The file's text, with every patch.
    pub(crate) fn render(&self) -> Result<String, Error> {
        self.policy
            .to_cil()
            .map_err(|source| Error::Render(Box::new(source)))
    }

    /// Whether the file with every patch compiles, as init compiles it at boot,
    /// after `others`, the device's other policy files (path, text).
    pub(crate) fn check(&self, others: &[(String, String)]) -> Result<(), Error> {
        let loaded = others
            .iter()
            .try_fold(CilPolicy::new(), |loaded, (path, text)| {
                loaded.add_file_named(path, text).map_err(at(path))
            })?
            .add_file_named(&self.name, &self.text)
            .map_err(at(&self.name))?;
        let mut loaded = self
            .patches
            .iter()
            .try_fold(loaded, |loaded, (name, rules)| {
                loaded.add_policy_named(name, rules).map_err(at(name))
            })?;
        loaded
            .compile()
            .map_err(|source| Error::Uncompiled(Box::new(source)))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A complete policy, as the platform's: libsemanage's smallest test policy.
    const PLATFORM: &str = "\
        (typeattribute cil_gen_require) (roleattribute cil_gen_require) (handleunknown allow)
        (mls true) (policycap network_peer_controls) (policycap open_perms)
        (sid security) (sidorder (security)) (sensitivity s0) (sensitivityorder (s0))
        (user system_u) (userrole system_u object_r) (userlevel system_u (s0))
        (userrange system_u ((s0) (s0))) (role object_r) (roletype object_r test_t)
        (type test_t) (sidcontext security (system_u object_r test_t ((s0) (s0))))
        (class test_class (test_perm)) (classorder (test_class))
        (allow test_t self (test_class (test_perm)))";
    /// The vendor's policy, which only resolves with the platform's.
    const VENDOR: &str = "(type vendor_t) (roletype object_r vendor_t)";

    fn platform() -> Vec<(String, String)> {
        vec![(
            "/system/etc/selinux/plat_sepolicy.cil".into(),
            PLATFORM.into(),
        )]
    }

    fn vendor() -> Document {
        Document::open("/vendor/vendor.cil".into(), VENDOR.into(), &platform()).unwrap()
    }

    #[test]
    fn a_patch_that_compiles_is_rendered_into_the_file_alone() {
        let mut document = vendor();
        let rules = "type hal_test_exec;\nallow vendor_t hal_test_exec:test_class test_perm;";
        document
            .add("a step: test.te", parse("test.te", rules).unwrap())
            .unwrap();
        document.check(&platform()).unwrap();
        let patched = document.render().unwrap();
        assert!(patched.contains("(type vendor_t)"), "{patched}");
        assert!(patched.contains("(type hal_test_exec)"), "{patched}");
        assert!(
            patched.contains("(allow vendor_t hal_test_exec (test_class (test_perm)))"),
            "{patched}"
        );
        assert!(
            !patched.contains("(type test_t)"),
            "the platform's policy is not written: {patched}"
        );
    }

    #[test]
    fn what_patches_and_the_other_files_declare_is_known() {
        let mut document = vendor();
        assert!(document.declared().contains("vendor_t"));
        assert!(
            document.declared().contains("test_t"),
            "the platform's types"
        );
        document
            .add(
                "a step: added.te",
                parse("added.te", "type added_t;").unwrap(),
            )
            .unwrap();
        assert!(document.declared().contains("added_t"));
    }

    #[test]
    fn a_patch_naming_a_type_nothing_defines_is_refused_by_its_input() {
        let mut document = vendor();
        let good = "type good_t;\nallow good_t vendor_t:test_class test_perm;";
        document
            .add("first step: good.te", parse("good.te", good).unwrap())
            .unwrap();
        let bad = "allow vendor_t missing_t:test_class test_perm;";
        document
            .add("second step: bad.te", parse("bad.te", bad).unwrap())
            .unwrap();
        let refused = document.check(&platform()).unwrap_err();
        assert!(matches!(refused, Error::Uncompiled(_)), "{refused:?}");
        assert_eq!(refused.input(), Some("second step: bad.te"));
    }

    #[test]
    fn a_name_two_patches_share_is_made_unique() {
        let mut document = vendor();
        let rules = || parse("a.te", "type one_t;").unwrap();
        document.add("a step: a.te", rules()).unwrap();
        document.add("a step: a.te", rules()).unwrap();
        document.check(&platform()).unwrap();
    }

    #[test]
    fn rendering_gives_the_same_bytes_every_time() {
        let rules = "type a_t;\ntype b_t;\nallow { a_t b_t } vendor_t:test_class { test_perm };";
        let render = || {
            let mut document = vendor();
            document
                .add("a step: t.te", parse("t.te", rules).unwrap())
                .unwrap();
            document.render().unwrap()
        };
        assert_eq!(render(), render());
    }

    #[test]
    fn taken_rules_are_parsed_as_they_are() {
        // binder_call is one of Android's macros: a taken rule is not expanded.
        let mut document = vendor();
        let rules = "type binder_call;\nallow vendor_t binder_call:test_class { test_perm };";
        document
            .add("a step: taken", parse("taken", rules).unwrap())
            .unwrap();
        let patched = document.render().unwrap();
        assert!(patched.contains("(type binder_call)"), "{patched}");
    }
}
