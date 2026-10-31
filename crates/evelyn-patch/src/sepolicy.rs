//! SELinux CIL policies patched with TE rules, through libsepol.

use crate::Context;
use android_firmware::RelativePath;
use std::{fs, path::Path};
use thiserror::Error;

/// The M4 macros TE policies are written with: Android's own, and the self
/// permissions a domain added to a compiled policy needs.
const MACROS: [&str; 3] = [
    "selinux/te_macros.m4",
    "selinux/self_domain_macros.m4",
    "selinux/global_macros.m4",
];

/// `bytes`, a CIL policy file, patched with the TE `policies` (the recipe's assets).
/// It must resolve with `resolve_with`, the device's other policy files.
pub(crate) fn apply(
    bytes: &[u8],
    policies: &[RelativePath],
    resolve_with: &[String],
    context: &Context<'_>,
) -> Result<Vec<u8>, crate::Error> {
    let text = |bytes: Vec<u8>| {
        String::from_utf8(bytes).map_err(|_| crate::Error::Unsupported("policies are UTF-8 text"))
    };
    let patches = policies
        .iter()
        .map(|path| {
            Ok((
                path.to_string(),
                text(fs::read(path.resolve(context.assets))?)?,
            ))
        })
        .collect::<Result<Vec<_>, crate::Error>>()?;
    let macros = MACROS
        .iter()
        .map(|name| context.tools.file(name))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(patch(&text(bytes.to_vec())?, resolve_with, &patches, &macros)?.into_bytes())
}

/// libsepol's errors are boxed: they are large, and would make every result as large.
#[derive(Debug, Error)]
pub enum Error {
    #[error("SELinux policy {name}: {source}")]
    Policy {
        name: String,
        #[source]
        source: Box<sepolicy::Error>,
    },
    #[error("the patched SELinux policy does not resolve with the device's other policy files")]
    Unresolved(#[source] Box<sepolicy::Error>),
    #[error("cannot render the patched SELinux policy: {0}")]
    Render(#[source] Box<sepolicy::Error>),
}

/// `policy` with every TE policy in `patches` (name, text) added, after expanding
/// the M4 macros in `macros`. The result must resolve when `resolve_with` is loaded
/// before it, as the device loads its other policy files.
fn patch(
    policy: &str,
    resolve_with: &[impl AsRef<str>],
    patches: &[(String, String)],
    macros: &[impl AsRef<Path>],
) -> Result<String, crate::Error> {
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
            let expanded = sepolicy::parser::expand(text, macros).map_err(at(name))?;
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
    use std::path::PathBuf;

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

    fn macros() -> Vec<PathBuf> {
        let tools = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools");
        MACROS.iter().map(|name| tools.join(name)).collect()
    }

    #[test]
    fn a_patch_that_resolves_is_added_to_the_policy_alone() {
        let rules = "type hal_test_exec;\nallow vendor_t hal_test_exec:test_class test_perm;";
        let patched = patch(
            VENDOR,
            &[PLATFORM],
            &[("test.te".into(), rules.into())],
            &macros(),
        )
        .unwrap();
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
        let refused = patch(
            VENDOR,
            &[PLATFORM],
            &[("test.te".into(), rules.into())],
            &macros(),
        );
        assert!(
            matches!(refused, Err(crate::Error::Sepolicy(Error::Unresolved(_)))),
            "{refused:?}"
        );
    }
}
