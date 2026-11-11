//! Each operation a recipe can name, over the files of the build or of the open
//! package: what [`crate::files`] selects, reads and writes.

use crate::{
    Composition, Error,
    compose::{Bound, Context},
    target::{Overrides, Placing},
};
use android_firmware::{DevicePath, Firmware, RelativePath, Sha256Digest};
use recipe::{
    Expr, Feature, Hex, Location, Operation, Returns, XmlEdit, escape_attribute,
    tree::Metadata,
    value::{Located, Place},
};
use std::collections::BTreeMap;

impl Composition {
    /// Apply one operation, with the values `bound` holds for the `{name}`s in it.
    pub(crate) fn run(
        &mut self,
        operation: &Operation,
        bound: &Bound,
        context: &Context<'_>,
    ) -> Result<(), Error> {
        if self.package.is_some() && !operation.in_package() {
            return Err(Error::OutsidePackage(operation.name()));
        }
        match operation {
            Operation::Copy {
                from,
                to,
                metadata,
                overwrite,
                create_parent,
            } => {
                let placing = Placing {
                    metadata: *metadata,
                    overwrite: *overwrite,
                    create_parent: *create_parent,
                };
                self.copy(from, to.as_ref(), placing, bound, context)
            }
            Operation::Import(from) => {
                let placing = Placing {
                    metadata: Metadata::Source,
                    overwrite: true,
                    create_parent: true,
                };
                self.copy(from, None, placing, bound, context)
            }
            Operation::Install { asset, to } => {
                let placing = Placing {
                    metadata: Metadata::Default,
                    overwrite: true,
                    create_parent: true,
                };
                let from = Location::Asset(asset.clone());
                self.copy(&from, Some(to), placing, bound, context)
            }
            Operation::Remove {
                pattern,
                origin,
                partition,
            } => self.remove(pattern, *origin, partition.as_deref(), bound, context),
            Operation::SetMetadata {
                pattern,
                uid,
                gid,
                mode,
                selinux,
            } => {
                let selinux = match selinux {
                    Some(label) => Some(recipe::substitute(label, bound)?),
                    None => None,
                };
                if selinux
                    .as_deref()
                    .is_some_and(|label| !recipe::tree::selinux_label(label))
                {
                    return Err(Error::Unsupported("a label is user:role:type:level"));
                }
                let overrides = Overrides {
                    uid: *uid,
                    gid: *gid,
                    mode: *mode,
                    selinux,
                };
                self.set_metadata(pattern, &overrides, bound, context)
            }
            Operation::SetProperty { key, value, target } => {
                self.set_property(key, value, target.as_ref(), bound, context)
            }
            Operation::RemoveProperty { key, target } => {
                self.remove_property(key, target.as_ref(), bound, context)
            }
            Operation::ApplyDiff { target, patch } => {
                let patch = self.asset(patch, bound, context)?;
                self.edit_named(target, bound, context, |bytes| {
                    Ok(crate::text::apply(&text(bytes)?, &patch)?.into_bytes())
                })
            }
            Operation::WriteBytes { target, edits } => {
                self.write_bytes(target, edits, bound, context)
            }
            Operation::EditXml { target, edits } => self.edit_xml(target, edits, bound, context),
            Operation::Package { target, edits } => self.package(target, edits, bound, context),
            Operation::StubMethod {
                class,
                method,
                returns,
            } => {
                let stub = match returns {
                    Returns::Void => smali::Stub::Void,
                    Returns::Zero => smali::Stub::Zero,
                    Returns::One => smali::Stub::One,
                };
                self.smali(class, method, bound, context, |source, class, method| {
                    smali::stub_method(source, class, method, stub)
                })
            }
            Operation::ReplaceMethod {
                class,
                method,
                body,
            } => {
                let body = self.asset(body, bound, context)?;
                self.smali(class, method, bound, context, |source, class, method| {
                    smali::replace_method(source, class, method, &body)
                })
            }
            Operation::InsertMethod {
                class,
                method,
                body,
            } => {
                let body = self.asset(body, bound, context)?;
                self.smali(class, method, bound, context, |source, class, method| {
                    smali::insert_method(source, class, method, &body)
                })
            }
            Operation::Assert { condition, message } => {
                match self.eval(condition, bound, context)?.as_bool()? {
                    true => Ok(()),
                    false => Err(Error::Assertion(recipe::substitute(message, bound)?)),
                }
            }
            Operation::AssertHash { target, sha256 } => {
                self.assert_hash(target, sha256, bound, context)
            }
            Operation::Append { target, content } => {
                let added = self.string(content, bound, context)?;
                self.edit_named(target, bound, context, |bytes| {
                    Ok(appended(&text(bytes)?, &added).into_bytes())
                })
            }
            Operation::AddPolicy { target, policies } => {
                self.add_policy(target, policies, bound, context)
            }
            Operation::EnableFeature(feature) => {
                let (file, root, element, name, args) = match feature {
                    Feature::Android(name) => (
                        "/my_product/etc/permissions/com.oplus.android-features.xml",
                        "permissions",
                        "feature",
                        name,
                        None,
                    ),
                    Feature::Oplus(name) => (
                        "/my_product/etc/permissions/oplus.feature.android.xml",
                        "oplus-config",
                        "oplus-feature",
                        name,
                        None,
                    ),
                    Feature::App { name, args, file } => (
                        file.as_str(),
                        "extend_features",
                        "app_feature",
                        name,
                        args.as_deref(),
                    ),
                };
                let args = args.map_or(String::new(), |args| {
                    format!(" args=\"{}\"", escape_attribute(args))
                });
                // Declared once: one already there is replaced.
                let edit = XmlEdit::PutChild {
                    parent: root.to_owned(),
                    key: feature_match(element, name),
                    element: format!("<{element} name=\"{}\"{args}/>", escape_attribute(name)),
                    create: true,
                };
                self.edit_xml(file, &[edit], bound, context)
            }
            // Only the files that mention a feature written out: one that cannot
            // hold it is not touched, or even parsed.
            Operation::DisableFeature(name) => {
                let mut files: Vec<Located> = Vec::new();
                for pattern in ["/*/etc/permissions/*.xml", "/*/etc/extension/*.xml"] {
                    for file in self.select_build(pattern, None, None, context)? {
                        if !files.contains(&file) {
                            files.push(file);
                        }
                    }
                }
                if let Some(name) = recipe::written_out(name) {
                    let mut mentioning = Vec::new();
                    for file in files {
                        let text = String::from_utf8(self.content(&file, context)?)
                            .map_err(|_| Error::Unsupported("Read reads UTF-8 text"))?;
                        if text.contains(&name) {
                            mentioning.push(file);
                        }
                    }
                    files = mentioning;
                }
                if files.is_empty() {
                    self.skip(format!("feature {name} is not declared"));
                    return Ok(());
                }
                let edits: Vec<XmlEdit> = ["feature", "oplus-feature", "app_feature"]
                    .into_iter()
                    .map(|element| XmlEdit::Remove {
                        path: format!("//{}", feature_match(element, name)),
                    })
                    .collect();
                let edits = xml_ops(&edits, bound)?;
                self.edit(&files, context, |bytes| {
                    Ok(crate::xml::apply(bytes, &edits)?)
                })
            }
        }
    }

    /// A recipe asset's text.
    fn asset(&self, path: &str, bound: &Bound, context: &Context<'_>) -> Result<String, Error> {
        let path = RelativePath::try_from(recipe::substitute(path, bound)?)?;
        text(&std::fs::read(context.asset(&path)?)?)
    }

    /// Copy what `from` names, which must be there, into the build or the open
    /// package, at the same path or at `to`.
    fn copy(
        &mut self,
        from: &Location,
        to: Option<&Expr>,
        placing: Placing,
        bound: &Bound,
        context: &Context<'_>,
    ) -> Result<(), Error> {
        let selected = self.select(from, bound, context)?;
        if selected.is_empty() {
            let path = from.path();
            return Err(Error::Assertion(match from {
                Location::Rom(_) => format!("{path} is not in the ROM"),
                Location::Stock(_) => format!("{path} is not in the stock firmware"),
                Location::Asset(_) => format!("{path} is not in the recipe's assets"),
                _ => format!("{path} is not there"),
            }));
        }
        let to = match to {
            Some(to) => Some(DevicePath::try_from(self.string(to, bound, context)?)?),
            None => None,
        };
        if to.is_some() && selected.len() > 1 {
            return Err(Error::Unsupported(
                "a copy of several entries keeps their paths",
            ));
        }
        for entry in &selected {
            if self.package.is_some() && entry.place != Place::Asset {
                return Err(Error::Unsupported("a package takes assets"));
            }
            match entry.place {
                Place::Firmware(firmware) => {
                    self.copy_firmware(firmware, entry, to.as_ref(), placing, context)?
                }
                Place::Asset => {
                    let to = to
                        .as_ref()
                        .ok_or(Error::Unsupported("an asset is copied to a path"))?;
                    self.copy_asset(&entry.path, to, placing, context)?;
                }
                Place::Build => {
                    let to = to
                        .as_ref()
                        .ok_or(Error::Unsupported("a copy within the build needs a path"))?;
                    self.copy_build(entry, to, placing, context)?;
                }
                Place::Package => {
                    return Err(Error::Unsupported("a package's files are not copied"));
                }
            }
        }
        Ok(())
    }

    /// Remove what `pattern` matches; matching nothing is reported, not an error.
    fn remove(
        &mut self,
        pattern: &str,
        origin: Option<Firmware>,
        partition: Option<&str>,
        bound: &Bound,
        context: &Context<'_>,
    ) -> Result<(), Error> {
        let path = recipe::substitute(pattern, bound)?;
        let selected = match self.package {
            Some(_) => self.select_package(&path)?,
            None => self.select_build(&path, origin, partition, context)?,
        };
        if selected.is_empty() {
            self.skip(format!("nothing to remove: {pattern}"));
        }
        for entry in &selected {
            self.remove_entry(entry)?;
        }
        Ok(())
    }

    /// Set metadata on every entry `pattern` matches, which must match something.
    fn set_metadata(
        &mut self,
        pattern: &str,
        overrides: &Overrides,
        bound: &Bound,
        context: &Context<'_>,
    ) -> Result<(), Error> {
        let selected = self.named(pattern, bound, context)?;
        if selected.is_empty() {
            return Err(Error::Assertion(format!("{pattern} is not there")));
        }
        for entry in selected {
            self.target(&entry.partition)?
                .set_metadata(&entry.path, overrides)?;
        }
        Ok(())
    }

    /// The property files a target names: those init loads, without one; else
    /// those it loads from a `Partition(…)`, or what a location selects.
    fn property_files(
        &self,
        target: Option<&Location>,
        bound: &Bound,
        context: &Context<'_>,
    ) -> Result<Vec<Located>, Error> {
        match target {
            None => Ok(self.loaded(context)?.files),
            Some(target) => self.select(target, bound, context),
        }
    }

    /// The files of `files` that define `key`.
    fn defining(
        &self,
        files: Vec<Located>,
        key: &str,
        context: &Context<'_>,
    ) -> Result<Vec<Located>, Error> {
        let mut defining = Vec::new();
        for file in files {
            let text = String::from_utf8_lossy(&self.content(&file, context)?).into_owned();
            if android_property::get(&text, key).is_some() {
                defining.push(file);
            }
        }
        Ok(defining)
    }

    /// Set a property in every file of the target's scope that defines it, else
    /// add it where a new one goes: a target's file; a partition's own; without a
    /// target the one init loads last, so it takes effect.
    fn set_property(
        &mut self,
        written: &str,
        value: &str,
        target: Option<&Location>,
        bound: &Bound,
        context: &Context<'_>,
    ) -> Result<(), Error> {
        let key = recipe::substitute(written, bound)?;
        let value = recipe::substitute(value, bound)?;
        if !recipe::property_key(&key) || value.contains('\n') {
            return Err(Error::Unsupported(
                "a property key and value are one line each",
            ));
        }
        let files = self.property_files(target, bound, context)?;
        let defining = self.defining(files, &key, context)?;
        if !defining.is_empty() {
            return self.edit(&defining, context, |bytes| {
                Ok(android_property::set(&text(bytes)?, &key, &value, false).into_bytes())
            });
        }
        let new = self.new_property_file(target, bound, context)?;
        if new.is_empty() {
            return Err(Error::Assertion(format!(
                "no property file to hold {written}"
            )));
        }
        self.edit(&new, context, |bytes| {
            Ok(android_property::set(&text(bytes)?, &key, &value, true).into_bytes())
        })
    }

    /// The file a new property goes to, if there is one.
    fn new_property_file(
        &self,
        target: Option<&Location>,
        bound: &Bound,
        context: &Context<'_>,
    ) -> Result<Vec<Located>, Error> {
        let candidates: Vec<(&str, Option<&str>)> = match target {
            Some(Location::Partition(partition)) => android_firmware::PARTITION_PROPERTY_FILES
                .iter()
                .map(|path| (*path, Some(partition.as_ref())))
                .collect(),
            Some(file) => return self.select(file, bound, context),
            // Last loaded first: within a group the canonical file, else the legacy
            // ones from the last loaded.
            None => android_firmware::PROPERTY_FILES
                .iter()
                .rev()
                .flat_map(|group| {
                    let (canonical, legacy) = group.split_first().expect("a file");
                    std::iter::once(canonical).chain(legacy.iter().rev())
                })
                .map(|path| (*path, None))
                .collect(),
        };
        for (path, partition) in candidates {
            let found = self.select_build(path, None, partition, context)?;
            if !found.is_empty() {
                return Ok(found);
            }
        }
        Ok(Vec::new())
    }

    /// Remove every definition of a property in the target's scope; where none is,
    /// that is reported.
    fn remove_property(
        &mut self,
        written: &str,
        target: Option<&Location>,
        bound: &Bound,
        context: &Context<'_>,
    ) -> Result<(), Error> {
        let key = recipe::substitute(written, bound)?;
        let files = self.property_files(target, bound, context)?;
        let defining = self.defining(files, &key, context)?;
        if defining.is_empty() {
            self.skip(format!("property {written} is not defined there"));
        }
        self.edit(&defining, context, |bytes| {
            Ok(android_property::remove(&text(bytes)?, &key).into_bytes())
        })
    }

    fn write_bytes(
        &mut self,
        target: &str,
        edits: &BTreeMap<u64, Hex>,
        bound: &Bound,
        context: &Context<'_>,
    ) -> Result<(), Error> {
        self.edit_named(target, bound, context, |bytes| {
            Ok(crate::binary::apply(bytes, edits)?)
        })
    }

    fn edit_xml(
        &mut self,
        target: &str,
        edits: &[XmlEdit],
        bound: &Bound,
        context: &Context<'_>,
    ) -> Result<(), Error> {
        let edits = xml_ops(edits, bound)?;
        self.edit_named(target, bound, context, |bytes| {
            Ok(crate::xml::apply(bytes, &edits)?)
        })
    }

    /// Decode a package, apply `edits` inside it and rebuild it.
    fn package(
        &mut self,
        target: &str,
        edits: &[Operation],
        bound: &Bound,
        context: &Context<'_>,
    ) -> Result<(), Error> {
        let path = DevicePath::try_from(recipe::substitute(target, bound)?)?;
        let at = self.resolve_one(&path, context)?;
        let opened = crate::package::open(self.target(&at.partition)?, &at, edits, context)?;
        self.package = Some(opened);
        let ran = edits
            .iter()
            .try_for_each(|edit| self.run(edit, bound, context));
        let opened = self.package.take().expect("opened above");
        ran?;
        crate::package::close(self.target(&at.partition)?, &at.path, opened, context)
    }

    /// Change one method of a class in the open package, in the smali file that
    /// defines it: `smali*/com/example/Name.smali`, whichever dex it is in.
    fn smali(
        &mut self,
        class: &str,
        method: &str,
        bound: &Bound,
        context: &Context<'_>,
        edit: impl Fn(&str, &str, &str) -> Result<String, smali::Error>,
    ) -> Result<(), Error> {
        let (class, method) = (
            recipe::substitute(class, bound)?,
            recipe::substitute(method, bound)?,
        );
        let name = class.trim_start_matches('L').trim_end_matches(';');
        let files = self.select_package(&format!("/smali*/{name}.smali"))?;
        if files.is_empty() {
            return Err(Error::Assertion(format!(
                "class {class} is not in the package"
            )));
        }
        self.edit(&files, context, |bytes| {
            Ok(edit(&text(bytes)?, &class, &method)?.into_bytes())
        })
    }

    fn assert_hash(
        &self,
        target: &str,
        sha256: &Sha256Digest,
        bound: &Bound,
        context: &Context<'_>,
    ) -> Result<(), Error> {
        let files = self.named(target, bound, context)?;
        let [file] = &files[..] else {
            return Err(Error::Unsupported("AssertHash needs exactly one file"));
        };
        match self.sha256(file, context)? == *sha256 {
            true => Ok(()),
            false => Err(Error::Assertion(format!(
                "{target} does not have SHA-256 {sha256}"
            ))),
        }
    }

    /// Patch a CIL policy file with TE policies, checked against the files init
    /// compiles the policy with: all but the one patched, those named for the
    /// platform policy version the vendor targets.
    fn add_policy(
        &mut self,
        target: &str,
        policies: &[String],
        bound: &Bound,
        context: &Context<'_>,
    ) -> Result<(), Error> {
        let named = self.select_build(android_firmware::SEPOLICY_VERSION, None, None, context)?;
        let [named] = &named[..] else {
            return Err(Error::Assertion(
                "the vendor does not name the platform policy version".into(),
            ));
        };
        let version = self.text(named, context)?.trim().to_owned();
        let patched = recipe::substitute(target, bound)?;
        let mut files = Vec::new();
        for (path, suffix) in android_firmware::SEPOLICY_FILES {
            let path = match suffix {
                None if path == patched => continue,
                None => path.to_owned(),
                Some(suffix) => format!("{path}{version}{suffix}"),
            };
            for file in self.select_build(&path, None, None, context)? {
                if !files.contains(&file) {
                    files.push(file);
                }
            }
        }
        let resolve_with = files
            .iter()
            .map(|file| {
                String::from_utf8(self.content(file, context)?)
                    .map_err(|_| Error::Unsupported("a policy file is UTF-8 text"))
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let policies = policies
            .iter()
            .map(|policy| Ok(RelativePath::try_from(recipe::substitute(policy, bound)?)?))
            .collect::<Result<Vec<_>, Error>>()?;
        self.edit_named(target, bound, context, |bytes| {
            crate::sepolicy::apply(bytes, &policies, &resolve_with, context)
        })
    }
}

/// XML edits with their strings' `{name}`s substituted, and parsed.
fn xml_ops(edits: &[XmlEdit], bound: &Bound) -> Result<Vec<recipe::XmlOp>, Error> {
    edits
        .iter()
        .map(|edit| {
            edit.map(|text| recipe::substitute(text, bound))?
                .parse()
                .map_err(|error| Error::from(recipe::Error::Invalid(error)))
        })
        .collect()
}

/// `<element name="name">`, as an XML selector written as a string.
fn feature_match(element: &str, name: &str) -> String {
    String::from(recipe::XmlMatch {
        name: recipe::XmlName {
            namespace: None,
            local: element.to_owned(),
        },
        predicate: Some(recipe::XmlPredicate::Attribute {
            name: recipe::XmlName {
                namespace: None,
                local: "name".to_owned(),
            },
            value: name.to_owned(),
        }),
    })
}

fn text(bytes: &[u8]) -> Result<String, Error> {
    String::from_utf8(bytes.to_vec()).map_err(|_| Error::Unsupported("text edits require UTF-8"))
}

/// `old` with `added` after it on lines of its own: a newline goes between them if
/// `old` does not end in one, and after `added` if it does not. Nothing added
/// leaves `old` as it is.
fn appended(old: &str, added: &str) -> String {
    if added.is_empty() {
        return old.to_owned();
    }
    let mut text = old.to_owned();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(added);
    if !added.ends_with('\n') {
        text.push('\n');
    }
    text
}
