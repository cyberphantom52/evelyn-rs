//! SELinux CIL policies patched with TE rules, through libsepol.

use crate::patch::Context;
use android::path::RelativePath;
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
            String::from_utf8(fs::read(context.tools.file(name)?)?)
                .map_err(|_| crate::patch::Error::Unsupported("policies are UTF-8 text"))
        })
        .collect()
}

/// `bytes`, a CIL policy file, patched with the TE `policies` (the recipe's assets).
/// It must resolve with `resolve_with`, the device's other policy files.
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
    let patches = policies
        .iter()
        .map(|path| Ok((path.to_string(), text(fs::read(context.asset(path)?)?)?)))
        .collect::<Result<Vec<_>, crate::patch::Error>>()?;
    Ok(patch(&text(bytes.to_vec())?, resolve_with, &patches, &macros(context)?)?.into_bytes())
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
    #[error("the patched SELinux policy does not resolve with the device's other policy files")]
    Unresolved(#[source] Box<sepolicy::Error>),
    #[error("cannot render the patched SELinux policy")]
    Render(#[source] Box<sepolicy::Error>),
}

/// `policy` with every TE policy in `patches` (name, text) added, after expanding
/// the M4 macros. The result must resolve when `resolve_with` is loaded
/// before it, as the device loads its other policy files.
fn patch(
    policy: &str,
    resolve_with: &[impl AsRef<str>],
    patches: &[(String, String)],
    macros: &[String],
) -> Result<String, crate::patch::Error> {
    let macros: Vec<&str> = macros.iter().map(String::as_str).collect();
    let at = |name: &str| {
        let name = name.to_owned();
        move |source| Error::Policy {
            name,
            source: Box::new(source),
        }
    };
    let rules = patches
        .iter()
        .map(|(name, text)| {
            let expanded = sepolicy::parser::expand_with(text, &macros).map_err(at(name))?;
            sepolicy::parser::parse(&expanded)
                .map_err(sepolicy::Error::from)
                .map_err(at(name))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let merged = |base: sepolicy::CilPolicy| {
        rules
            .iter()
            .try_fold(base.add_file(policy)?, |merged, rules| {
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
        .validate()
        .map_err(|source| Error::Unresolved(Box::new(source)))?;
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
    fn a_patch_that_resolves_is_added_to_the_policy_alone() {
        let rules = "type hal_test_exec;\nallow vendor_t hal_test_exec:test_class test_perm;";
        let patched = patch(VENDOR, &[PLATFORM], &[("test.te".into(), rules.into())], &[]).unwrap();
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
    fn a_patch_naming_a_type_nothing_defines_is_refused() {
        let rules = "allow vendor_t missing_t:test_class test_perm;";
        let refused = patch(VENDOR, &[PLATFORM], &[("test.te".into(), rules.into())], &[]);
        assert!(
            matches!(
                refused,
                Err(crate::patch::Error::Sepolicy(Error::Unresolved(_)))
            ),
            "{refused:?}"
        );
    }
}
