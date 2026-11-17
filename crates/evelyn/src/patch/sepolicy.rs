//! SELinux CIL policy files, patched with TE rules through libsepol.
//!
//! A policy file a step patches is open in its target until the build is
//! written. Each patch is added under a name of its own: a TE asset by its path,
//! an import by what it takes and from where. Once every step is applied, the file
//! is compiled with the device's other policy files, as init compiles it at boot,
//! so an error names the patch it is in.

use crate::patch::Context;
use crate::patch::target::IoAt;
use android::path::RelativePath;
use sepolicy::CilPolicy;
use sepolicy::parser::ast::Policy;
use std::collections::HashSet;
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

/// A CIL policy file open for patching: its text when opened, and the patches
/// steps added since, each under a name of its own.
pub(crate) struct Document {
    /// The file's device path, which its own text is loaded under.
    name: String,
    text: String,
    patches: Vec<(String, Policy)>,
    /// The types, attributes and aliases this file, its patches and the device's
    /// other policy files declare, which an import does not declare again.
    declared: HashSet<String>,
}

impl Document {
    /// Open the policy file at `name`, `text`, with `others`, the device's other
    /// policy files (path, text).
    pub(crate) fn open(
        name: String,
        text: String,
        others: &[(String, String)],
    ) -> Result<Self, Error> {
        let mut declared: HashSet<String> = CilPolicy::new()
            .add_file_named(&name, &text)
            .and_then(|policy| policy.declared_types())
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
            declared,
        })
    }

    pub(crate) fn declared(&self) -> &HashSet<String> {
        &self.declared
    }

    /// Add `rules` under `name`, made unique so an error names one patch.
    pub(crate) fn add(&mut self, name: &str, rules: Policy) -> Result<(), Error> {
        let mut unique = name.to_owned();
        for n in 2.. {
            if !self.patches.iter().any(|(taken, _)| *taken == unique) {
                break;
            }
            unique = format!("{name} ({n})");
        }
        let added = CilPolicy::new()
            .add_policy(&rules)
            .and_then(|added| added.declared_types())
            .map_err(at(&unique))?;
        self.declared.extend(added);
        self.patches.push((unique, rules));
        Ok(())
    }

    /// The file's text with every patch, loaded after `before`.
    fn load(&self, before: CilPolicy) -> Result<CilPolicy, Error> {
        let loaded = before
            .add_file_named(&self.name, &self.text)
            .map_err(at(&self.name))?;
        self.patches
            .iter()
            .try_fold(loaded, |loaded, (name, rules)| {
                loaded.add_policy_named(name, rules).map_err(at(name))
            })
    }

    /// The file's content: its text with every patch.
    pub(crate) fn render(&self) -> Result<String, Error> {
        self.load(CilPolicy::new())?
            .to_cil()
            .map_err(|source| Error::Render(Box::new(source)))
    }

    /// Compile the file with every patch, as init compiles it at boot, after
    /// `others`, the device's other policy files (path, text).
    pub(crate) fn check(&self, others: &[(String, String)]) -> Result<(), Error> {
        let before = others
            .iter()
            .try_fold(CilPolicy::new(), |loaded, (path, text)| {
                loaded.add_file_named(path, text).map_err(at(path))
            })?;
        self.load(before)?
            .compile()
            .map_err(|source| Error::Uncompiled(Box::new(source)))?;
        Ok(())
    }
}
