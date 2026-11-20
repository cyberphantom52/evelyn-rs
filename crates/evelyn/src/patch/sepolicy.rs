//! SELinux CIL policy files, patched with TE rules through libsepol.
//!
//! A policy file a step patches is open in its target until the build is
//! written. Each patch is added under a name of its own: a TE asset by its path,
//! an import by what it takes and from where. Once every step is applied, the file
//! is compiled with the device's other policy files, as init compiles it at boot,
//! so an error names the patch it is in. A rule an import left out, needing a
//! name nothing defined then, is held in the file too: it goes in once a later
//! step defines every name it needs, under the step that held it.

use crate::patch::Context;
use crate::patch::target::IoAt;
use android::path::RelativePath;
use sepolicy::CilPolicy;
use sepolicy::parser::ast::Policy;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::PathBuf;
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
    /// The rules imports left out, which go in once the file declares every
    /// name they need.
    held: Vec<Held>,
}

/// A rule the import at a recipe's step left out, and the names it needs that
/// nothing defined then.
#[derive(Clone, Debug)]
pub(crate) struct Held {
    pub(crate) recipe: PathBuf,
    pub(crate) step: String,
    pub(crate) rule: String,
    pub(crate) needs: BTreeSet<String>,
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
            held: Vec::new(),
        })
    }

    pub(crate) fn declared(&self) -> &HashSet<String> {
        &self.declared
    }

    /// Hold `held` until the file declares every name it needs.
    pub(crate) fn hold(&mut self, held: Held) {
        self.held.push(held);
    }

    /// The held rules that still need a name the file does not declare.
    pub(crate) fn unresolved(&self) -> impl Iterator<Item = &Held> {
        self.held.iter().filter(|held| !self.resolves(held))
    }

    /// Whether the file declares every name `held` needs.
    fn resolves(&self, held: &Held) -> bool {
        held.needs.iter().all(|name| self.declared.contains(name))
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

    /// The file's text with every patch, loaded after `before`, then the held
    /// rules whose names it declares, each step's under a name of its own.
    fn load(&self, before: CilPolicy) -> Result<CilPolicy, Error> {
        let loaded = before
            .add_file_named(&self.name, &self.text)
            .map_err(at(&self.name))?;
        let loaded = self
            .patches
            .iter()
            .try_fold(loaded, |loaded, (name, rules)| {
                loaded.add_policy_named(name, rules).map_err(at(name))
            })?;
        let mut resolved: BTreeMap<(&PathBuf, &str), Vec<&str>> = BTreeMap::new();
        for held in self.held.iter().filter(|held| self.resolves(held)) {
            resolved
                .entry((&held.recipe, &held.step))
                .or_default()
                .push(&held.rule);
        }
        resolved
            .into_iter()
            .try_fold(loaded, |loaded, ((recipe, step), rules)| {
                let name = format!("{} step {step} (held)", recipe.display());
                tracing::debug!(policy = %self.name, %name, "held policy added:\n{}", rules.join("\n"));
                let rules = parse(&name, &rules.join("\n"))?;
                loaded.add_policy_named(&name, &rules).map_err(at(&name))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn held(rule: &str, needs: &[&str]) -> Held {
        Held {
            recipe: PathBuf::from("patches/a/patch.ron"),
            step: "a".to_owned(),
            rule: rule.to_owned(),
            needs: needs.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    fn document() -> Document {
        Document::open(
            "vendor_sepolicy.cil".to_owned(),
            "(typeattribute client)".to_owned(),
            &[],
        )
        .expect("the policy opens")
    }

    #[test]
    fn holds_a_rule_until_a_later_patch_declares_its_name() {
        let mut document = document();
        document.hold(held("typeattribute daemon client;", &["daemon"]));
        assert_eq!(document.unresolved().count(), 1);
        assert!(!document.render().expect("renders").contains("daemon"));

        document
            .add("b.te", parse("b.te", "type daemon;").expect("parses"))
            .expect("adds");
        assert_eq!(document.unresolved().count(), 0);
        let rendered = document.render().expect("renders");
        assert!(
            rendered.contains("(typeattributeset client (daemon))"),
            "{rendered}"
        );
    }

    #[test]
    fn a_rule_needing_two_names_waits_for_both() {
        let mut document = document();
        document.hold(held(
            "allow daemon server:file read;",
            &["daemon", "server"],
        ));
        document
            .add("b.te", parse("b.te", "type daemon;").expect("parses"))
            .expect("adds");
        assert_eq!(document.unresolved().count(), 1);
        assert!(!document.render().expect("renders").contains("server"));

        document
            .add("c.te", parse("c.te", "type server;").expect("parses"))
            .expect("adds");
        assert_eq!(document.unresolved().count(), 0);
        assert!(
            document
                .render()
                .expect("renders")
                .contains("daemon server")
        );
    }
}
