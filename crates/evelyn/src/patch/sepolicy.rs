//! SELinux CIL policies patched with TE rules, through libsepol.

use crate::patch::Context;
use crate::patch::target::IoAt;
use android::path::RelativePath;
use sepolicy::parser::ast::Policy;
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

/// `bytes`, a CIL policy file, patched with the TE `policies` (the recipe's assets).
/// It must compile with `resolve_with`, the device's other policy files.
pub(crate) fn apply(
    bytes: &[u8],
    policies: &[RelativePath],
    resolve_with: &[String],
    context: &Context<'_>,
) -> Result<Vec<u8>, crate::patch::Error> {
    let text = |bytes: Vec<u8>| {
        String::from_utf8(bytes)
            .map_err(|_| crate::patch::Error::Unsupported("policies are UTF-8 text"))
    };
    let macros = macros(context)?;
    let patches = policies
        .iter()
        .map(|path| {
            let file = context.asset(path)?;
            let name = path.to_string();
            let rules = expanded(&name, &text(fs::read(&file).at(&file)?)?, &macros)?;
            Ok((name, rules))
        })
        .collect::<Result<Vec<_>, crate::patch::Error>>()?;
    Ok(patch(&text(bytes.to_vec())?, resolve_with, &patches)?.into_bytes())
}

/// `bytes`, a CIL policy file, patched with `rules`, TE taken from another
/// policy, named `name` in errors. Taken rules have no macros, so they are not
/// expanded: a type named like a macro stays a type. The result must compile
/// with `resolve_with`, the device's other policy files.
pub(crate) fn import(
    bytes: &[u8],
    name: &str,
    rules: &str,
    resolve_with: &[String],
) -> Result<Vec<u8>, crate::patch::Error> {
    let parsed = sepolicy::parser::parse(rules)
        .map_err(sepolicy::Error::from)
        .map_err(at(name))?;
    let policy = String::from_utf8(bytes.to_vec())
        .map_err(|_| crate::patch::Error::Unsupported("policies are UTF-8 text"))?;
    Ok(patch(&policy, resolve_with, &[(name.to_owned(), parsed)])?.into_bytes())
}

/// The TE policy `text`, named `name` in errors, with the M4 `macros` expanded.
fn expanded(name: &str, text: &str, macros: &[String]) -> Result<Policy, Error> {
    let macros: Vec<&str> = macros.iter().map(String::as_str).collect();
    let expanded = sepolicy::parser::expand_with(text, &macros).map_err(at(name))?;
    sepolicy::parser::parse(&expanded)
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

/// `policy` with every TE policy in `patches` (name, parsed rules) added. The
/// result must compile, as init compiles it at boot, when `resolve_with` is
/// loaded before it.
fn patch(
    policy: &str,
    resolve_with: &[impl AsRef<str>],
    patches: &[(String, Policy)],
) -> Result<String, crate::patch::Error> {
    let merged = |base: sepolicy::CilPolicy| {
        patches
            .iter()
            .try_fold(base.add_file(policy)?, |merged, (_, rules)| {
                merged.add_policy(rules)
            })
    };
    let mut checked = resolve_with
        .iter()
        .try_fold(sepolicy::CilPolicy::new(), |loaded, text| {
            loaded.add_file(text.as_ref())
        })
        .map_err(at("resolve_with"))?;
    checked = merged(checked).map_err(at("the patched policy"))?;
    checked
        .compile()
        .map_err(|source| Error::Uncompiled(Box::new(source)))?;
    Ok(merged(sepolicy::CilPolicy::new())
        .map_err(at("the patched policy"))?
        .to_cil()
        .map_err(|source| Error::Render(Box::new(source)))?)
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

    #[test]
    fn a_patch_that_compiles_is_added_to_the_policy_alone() {
        let rules = "type hal_test_exec;\nallow vendor_t hal_test_exec:test_class test_perm;";
        let rules = expanded("test.te", rules, &[]).unwrap();
        let patched = patch(VENDOR, &[PLATFORM], &[("test.te".into(), rules)]).unwrap();
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
    fn imported_rules_are_added_as_they_are() {
        // binder_call is one of Android's macros: an imported rule is not expanded.
        let rules = "type binder_call;\nallow vendor_t binder_call:test_class { test_perm };";
        let imported = import(VENDOR.as_bytes(), "taken.te", rules, &[PLATFORM.into()]).unwrap();
        let imported = String::from_utf8(imported).unwrap();
        assert!(imported.contains("(type binder_call)"), "{imported}");
        assert!(
            imported.contains("(allow vendor_t binder_call (test_class (test_perm)))"),
            "{imported}"
        );
    }

    #[test]
    fn a_patch_naming_a_type_nothing_defines_is_refused() {
        let rules = "allow vendor_t missing_t:test_class test_perm;";
        let rules = expanded("test.te", rules, &[]).unwrap();
        let refused = patch(VENDOR, &[PLATFORM], &[("test.te".into(), rules)]);
        assert!(
            matches!(
                refused,
                Err(crate::patch::Error::Sepolicy(Error::Uncompiled(_)))
            ),
            "{refused:?}"
        );
    }
}
