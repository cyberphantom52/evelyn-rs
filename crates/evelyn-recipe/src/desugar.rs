//! Surface recipes (steps of operations, `let:` and `when:`) into the primitive tree.

use crate::{
    Error, Expr as Written, Feature, Location, Metadata, Operation, Recipe, Step, XmlEdit,
    XmlMatch, XmlName, XmlPath, XmlPredicate, escape_attribute,
    tree::{Assigned, Expr, MethodEdit, Pattern, Scope, Source, Stmt, Transform, Tree},
    value::Value,
};
use android_firmware::{DevicePath, Name, RelativePath};

impl Recipe {
    /// The recipe as primitives, its steps in dependency order.
    pub fn tree(&self) -> Result<Tree, Error> {
        Ok(Tree {
            name: self.name.clone(),
            body: self.ordered_steps()?.into_iter().map(step).collect(),
        })
    }
}

/// `let:` bindings enclose `when:`, which encloses the operations; a binding with
/// no value skips the step.
fn step(step: &Step) -> Stmt {
    let mut body: Vec<Stmt> = step.operations.iter().flat_map(operation).collect();
    if let Some(condition) = &step.when {
        body = vec![Stmt::When {
            condition: expr(condition),
            body,
            skip: format!("condition does not hold: {}", shown(condition)),
        }];
    }
    for (name, value) in step.bindings.iter().rev() {
        body = vec![Stmt::Let {
            name: name.clone(),
            value: expr(value),
            body: vec![Stmt::When {
                condition: defined(Expr::Var(name.clone())),
                body,
                skip: format!("{name} is not set: {}", shown(value)),
            }],
        }];
    }
    Stmt::Step {
        id: step.id.clone(),
        body,
    }
}

/// An expression as reports name it: a string as it is, anything else in RON.
fn shown(written: &Written) -> String {
    match written {
        Written::Text(text) => text.clone(),
        other => ron::to_string(other).unwrap_or_default(),
    }
}

/// A written expression as primitives.
fn expr(written: &Written) -> Expr {
    let boxed = |inner: &Written| Box::new(expr(inner));
    let all = |exprs: &[Written]| exprs.iter().map(expr).collect();
    match written {
        Written::Exists(inner) => match &**inner {
            Written::At(Location::Partition(name)) => Expr::HasPartition(name.clone()),
            inner => Expr::Exists(Box::new(expr(inner))),
        },
        Written::At(location) => at(location, Scope::default()),
        Written::Read(inner) => Expr::Read(boxed(inner)),
        Written::Hash(inner) => Expr::Hash(boxed(inner)),
        Written::Below(inner) => Expr::Below(boxed(inner)),
        Written::Property { key, target } => Expr::Property {
            key: key.clone(),
            files: target.as_ref().map(|_| Box::new(property_files(target))),
        },
        Written::Not(inner) => Expr::Not(boxed(inner)),
        Written::All(exprs) => Expr::All(all(exprs)),
        Written::Any(exprs) => Expr::Any(all(exprs)),
        Written::Union(exprs) => Expr::Union(all(exprs)),
        Written::Eq(left, right) => Expr::Eq(boxed(left), boxed(right)),
        Written::In(left, right) => Expr::In(boxed(left), boxed(right)),
        Written::If {
            condition,
            then,
            otherwise,
        } => Expr::If {
            condition: boxed(condition),
            then: boxed(then),
            otherwise: boxed(otherwise),
        },
        Written::Filter { over, name, keep } => Expr::Filter {
            over: boxed(over),
            name: name.clone(),
            keep: boxed(keep),
        },
        Written::Lines(inner) => Expr::Lines(boxed(inner)),
        Written::Join(inner) => Expr::Join(boxed(inner)),
        Written::Trim(inner) => Expr::Trim(boxed(inner)),
        Written::Matches(text, pattern) => Expr::Matches {
            text: boxed(text),
            pattern: pattern.clone(),
        },
        Written::Text(written) => text(written),
        Written::Bool(value) => Expr::Literal(Value::Bool(*value)),
        Written::List(values) => Expr::Literal(Value::List(values.clone())),
    }
}

/// A written string: each `{name}` in it, a name of `[A-Za-z0-9_-]`, is the value
/// bound to `name`; any other `{` stands for itself.
fn text(written: &str) -> Expr {
    let mut parts = Vec::new();
    let mut literal = String::new();
    let mut rest = written;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let end = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .unwrap_or(after.len());
        let bound = (end > 0 && after[end..].starts_with('}'))
            .then(|| Name::try_from(after[..end].to_owned()).ok())
            .flatten();
        match bound {
            Some(bound) => {
                literal.push_str(&rest[..start]);
                if !literal.is_empty() {
                    parts.push(Expr::Literal(Value::Str(std::mem::take(&mut literal))));
                }
                parts.push(Expr::Var(bound));
                rest = &after[end + 1..];
            }
            None => {
                literal.push_str(&rest[..=start]);
                rest = after;
            }
        }
    }
    literal.push_str(rest);
    if !literal.is_empty() || parts.is_empty() {
        parts.push(Expr::Literal(Value::Str(literal)));
    }
    match parts.len() {
        1 => parts.remove(0),
        _ => Expr::Concat(parts),
    }
}

/// What a location selects, within `scope` in the build: in a firmware or the
/// recipe's assets, or in the build or the open package, where a `Select` looks
/// unless told otherwise. A partition is its property files.
fn at(location: &Location, scope: Scope) -> Expr {
    let inside = match location {
        Location::Rom(_) => Some(Source::Rom),
        Location::Stock(_) => Some(Source::Stock),
        Location::Asset(_) => Some(Source::Assets),
        Location::Build(_) | Location::Package(_) => None,
        Location::Partition(name) => {
            return Expr::PropertyFiles {
                partition: Some(name.clone()),
            };
        }
    };
    select_expr(text(location.path()), Scope { inside, ..scope })
}

fn select(pattern: &str, scope: Scope) -> Expr {
    select_expr(Expr::Literal(Value::Str(pattern.to_owned())), scope)
}

fn select_expr(pattern: Expr, scope: Scope) -> Expr {
    Expr::Select {
        pattern: Box::new(pattern),
        scope,
    }
}

fn operation(operation: &Operation) -> Vec<Stmt> {
    let stmt = match operation {
        Operation::Copy { from, to, metadata } => {
            let path = from.path();
            let found = expect(
                at(from, Scope::default()),
                match from {
                    Location::Rom(_) => format!("{path} is not in the ROM"),
                    Location::Stock(_) => format!("{path} is not in the stock firmware"),
                    Location::Asset(_) => format!("{path} is not in the recipe's assets"),
                    _ => format!("{path} is not there"),
                },
            );
            let copy = Stmt::Copy {
                selection: at(from, Scope::default()),
                to: to.as_ref().map(expr),
            };
            // What the copy put in the build, by where it went.
            let placed = match to {
                Some(to) => shown(to),
                None => path.to_owned(),
            };
            let in_build = || match to {
                Some(to) => select_expr(expr(to), Scope::default()),
                None => select_expr(text(path), Scope::default()),
            };
            let defaults = || Stmt::SetMetadata {
                selection: Expr::Below(Box::new(in_build())),
                pattern: placed.clone(),
                metadata: Assigned::Default,
            };
            return match metadata {
                Metadata::Source => vec![found, copy],
                Metadata::Default => vec![found, copy, defaults()],
            };
        }
        Operation::Remove {
            pattern,
            origin,
            partition,
        } => {
            let selection = at(
                pattern,
                Scope {
                    origin: *origin,
                    partition: partition.clone(),
                    ..Scope::default()
                },
            );
            Stmt::When {
                condition: defined(selection.clone()),
                body: vec![Stmt::Remove { selection }],
                skip: format!("nothing to remove: {pattern}"),
            }
        }
        Operation::SetMetadata { pattern, .. } => Stmt::SetMetadata {
            selection: at(pattern, Scope::default()),
            pattern: pattern.to_string(),
            metadata: Assigned::Fields(crate::set(operation)),
        },
        // Where the key is defined, everywhere in the target's scope; or added
        // where a new one goes.
        Operation::SetProperty { key, value, target } => {
            let files = or_else(defining(key, target), new_property_file(target));
            return vec![
                expect(files.clone(), format!("no property file to hold {key}")),
                Stmt::Edit {
                    selection: files,
                    transform: Transform::Property {
                        key: key.clone(),
                        value: Some(value.clone()),
                    },
                },
            ];
        }
        // Every definition, or a lower one would show.
        Operation::RemoveProperty { key, target } => Stmt::When {
            condition: defined(defining(key, target)),
            body: vec![Stmt::Edit {
                selection: defining(key, target),
                transform: Transform::Property {
                    key: key.clone(),
                    value: None,
                },
            }],
            skip: format!("property {key} is not defined there"),
        },
        Operation::ApplyDiff { target, patch } => Stmt::Edit {
            selection: at(target, Scope::default()),
            transform: Transform::ApplyDiff(asset(patch)),
        },
        Operation::WriteBytes { target, edits } => Stmt::Edit {
            selection: at(target, Scope::default()),
            transform: Transform::WriteBytes(edits.clone()),
        },
        Operation::EditXml { target, edits } => Stmt::Edit {
            selection: at(target, Scope::default()),
            transform: Transform::Xml(edits.clone()),
        },
        Operation::Package { target, edits } => Stmt::Package {
            package: DevicePath::try_from(target.path().to_owned())
                .expect("a Package target is checked to be written out"),
            body: edits.iter().flat_map(self::operation).collect(),
        },
        Operation::StubMethod {
            class,
            method,
            returns,
        } => smali(class, method, MethodEdit::Stub(*returns)),
        Operation::ReplaceMethod {
            class,
            method,
            body,
        } => smali(class, method, MethodEdit::Replace(asset(body))),
        Operation::InsertMethod {
            class,
            method,
            body,
        } => smali(class, method, MethodEdit::Insert(asset(body))),
        Operation::EnableFeature(feature) => {
            let file = feature_file(feature);
            let (root, element, name, args) = match feature {
                Feature::Android(name) => ("permissions", "feature", name, None),
                Feature::Oplus(name) => ("oplus-config", "oplus-feature", name, None),
                Feature::App { name, args, .. } => {
                    ("extend_features", "app_feature", name, args.as_deref())
                }
            };
            let args = args.map_or(String::new(), |args| {
                format!(" args=\"{}\"", escape_attribute(args))
            });
            Stmt::Edit {
                selection: select(file, Scope::default()),
                transform: Transform::Xml(vec![XmlEdit::PutChild {
                    parent: XmlPath::try_from(root.to_owned()).expect("a literal XML path"),
                    key: feature_match(element, name),
                    element: format!("<{element} name=\"{}\"{args}/>", escape_attribute(name)),
                }]),
            }
        }
        Operation::DisableFeature(name) => {
            let edits: Vec<XmlEdit> = ["feature", "oplus-feature", "app_feature"]
                .into_iter()
                .map(|element| XmlEdit::Remove {
                    path: XmlPath::anywhere(
                        feature_match(element, name),
                        format!("//{element}[@name='{name}']"),
                    ),
                })
                .collect();
            // Only the files that mention the feature: one that cannot hold it is
            // not touched, or even parsed. A feature declared nowhere skips the step.
            let file = self::name("file");
            let files = Expr::Filter {
                over: Box::new(Expr::Union(vec![
                    select("/*/etc/permissions/*.xml", Scope::default()),
                    select("/*/etc/extension/*.xml", Scope::default()),
                ])),
                name: file.clone(),
                keep: Box::new(Expr::Matches {
                    text: Box::new(Expr::Read(Box::new(Expr::Var(file)))),
                    pattern: Pattern::literal(name),
                }),
            };
            Stmt::When {
                condition: defined(files.clone()),
                body: vec![Stmt::Edit {
                    selection: files,
                    transform: Transform::Xml(edits),
                }],
                skip: format!("feature {name} is not declared"),
            }
        }
        // Checked against the files init compiles the policy from, named for the
        // platform policy version the vendor targets: a step's binding, a token.
        Operation::AddPolicy { target, policies } => {
            let version = name("version");
            let str = |text: &str| Expr::Literal(Value::Str(text.to_owned()));
            let files = android_firmware::SEPOLICY_FILES
                .iter()
                .map(|(path, suffix)| match suffix {
                    None => select(path, Scope::default()),
                    Some(suffix) => select_expr(
                        Expr::Concat(vec![str(path), Expr::Var(version.clone()), str(suffix)]),
                        Scope::default(),
                    ),
                })
                .collect();
            let named = select(android_firmware::SEPOLICY_VERSION, Scope::default());
            return vec![
                expect(
                    named.clone(),
                    "the vendor does not name the platform policy version".into(),
                ),
                Stmt::Let {
                    name: version,
                    value: Expr::Trim(Box::new(Expr::Read(Box::new(named)))),
                    body: vec![Stmt::Edit {
                        selection: at(target, Scope::default()),
                        transform: Transform::AddPolicy {
                            policies: policies.iter().map(asset).collect(),
                            resolve_with: Expr::Union(files),
                        },
                    }],
                },
            ];
        }
        Operation::Append { target, content } => {
            let file = at(target, Scope::default());
            Stmt::Edit {
                transform: Transform::Write(appended(
                    Expr::Read(Box::new(file.clone())),
                    &expr(content),
                )),
                selection: file,
            }
        }
        Operation::AssertHash { target, sha256 } => Stmt::Assert {
            condition: Expr::Eq(
                Box::new(Expr::Hash(Box::new(at(target, Scope::default())))),
                Box::new(Expr::Literal(Value::Str(sha256.to_string()))),
            ),
            message: format!("{target} does not have SHA-256 {sha256}"),
        },
    };
    // An operation on one named file, or class, needs it there.
    let named = |target: String| {
        let message = format!("{target} is not there");
        (select(&target, Scope::default()), message)
    };
    let (selection, message) = match operation {
        Operation::SetMetadata {
            pattern: target, ..
        }
        | Operation::ApplyDiff { target, .. }
        | Operation::WriteBytes { target, .. }
        | Operation::EditXml { target, .. }
        | Operation::Append { target, .. }
        | Operation::AddPolicy { target, .. } => named(target.to_string()),
        Operation::EnableFeature(feature) => named(feature_file(feature).to_owned()),
        Operation::StubMethod { class, .. }
        | Operation::ReplaceMethod { class, .. }
        | Operation::InsertMethod { class, .. } => (
            smali_file(class),
            format!("class {class} is not in the package"),
        ),
        _ => return vec![stmt],
    };
    vec![expect(selection, message), stmt]
}

/// An asset's path in the recipe's directory, checked to be written out.
fn asset(location: &Location) -> RelativePath {
    RelativePath::try_from(location.path().to_owned()).expect("an asset path is checked")
}

/// The file a feature of its kind is declared in.
fn feature_file(feature: &Feature) -> &str {
    match feature {
        Feature::Android(_) => "/my_product/etc/permissions/com.oplus.android-features.xml",
        Feature::Oplus(_) => "/my_product/etc/permissions/oplus.feature.android.xml",
        Feature::App { file, .. } => file.path(),
    }
}

/// The property files a target names: those init loads, or those of them in a
/// partition, or one file.
fn property_files(target: &Option<Location>) -> Expr {
    match target {
        None => Expr::PropertyFiles { partition: None },
        Some(target) => at(target, Scope::default()),
    }
}

/// The property files in a target's scope that define `key`.
fn defining(key: &str, target: &Option<Location>) -> Expr {
    let file = name("file");
    Expr::Filter {
        over: Box::new(property_files(target)),
        name: file.clone(),
        keep: Box::new(Expr::Exists(Box::new(Expr::Property {
            key: key.to_owned(),
            files: Some(Box::new(Expr::Var(file))),
        }))),
    }
}

/// The file a new key goes to: a target's file; a partition's own; without a
/// target the one init loads last, so the key takes effect.
fn new_property_file(target: &Option<Location>) -> Expr {
    let first = |files: Vec<Expr>| {
        files
            .into_iter()
            .rev()
            .reduce(|otherwise, first| or_else(first, otherwise))
            .expect("files to choose from")
    };
    match target {
        Some(Location::Partition(partition)) => first(
            android_firmware::PARTITION_PROPERTY_FILES
                .iter()
                .map(|path| {
                    select(
                        path,
                        Scope {
                            partition: Some(partition.to_string()),
                            ..Scope::default()
                        },
                    )
                })
                .collect(),
        ),
        Some(file) => at(file, Scope::default()),
        // Last loaded first: within a group the canonical file, else the legacy
        // ones from the last loaded.
        None => first(
            android_firmware::PROPERTY_FILES
                .iter()
                .rev()
                .flat_map(|group| {
                    let (canonical, legacy) = group.split_first().expect("a file");
                    std::iter::once(canonical).chain(legacy.iter().rev())
                })
                .map(|path| select(path, Scope::default()))
                .collect(),
        ),
    }
}

/// An edit of one method of a class, in its smali file.
fn smali(class: &str, method: &str, edit: MethodEdit) -> Stmt {
    Stmt::Edit {
        selection: smali_file(class),
        transform: Transform::Smali {
            class: class.to_owned(),
            method: method.to_owned(),
            edit,
        },
    }
}

/// The smali file defining `Lcom/example/Name;` inside a package:
/// `smali*/com/example/Name.smali`, whichever dex it is in.
fn smali_file(class: &str) -> Expr {
    let name = class.trim_start_matches('L').trim_end_matches(';');
    select(&format!("/smali*/{name}.smali"), Scope::default())
}

/// The selection is not empty.
fn defined(selection: Expr) -> Expr {
    Expr::Exists(Box::new(selection))
}

/// `first` if it selects anything, else `otherwise`.
fn or_else(first: Expr, otherwise: Expr) -> Expr {
    let selected = name("selected");
    Expr::Let {
        name: selected.clone(),
        value: Box::new(first),
        body: Box::new(Expr::If {
            condition: Box::new(defined(Expr::Var(selected.clone()))),
            then: Box::new(Expr::Var(selected)),
            otherwise: Box::new(otherwise),
        }),
    }
}

/// `old` with `added` after it on lines of its own: a newline goes between them if
/// `old` does not end in one, and after `added` if it does not. Nothing added
/// leaves `old` as it is.
fn appended(old: Expr, added: &Expr) -> Expr {
    let (old_name, added_name) = (name("old"), name("added"));
    let (old, added) = ((old_name.clone(), old), (added_name.clone(), added.clone()));
    let var = |name: &Name| Expr::Var(name.clone());
    let str = |text: &str| Expr::Literal(Value::Str(text.to_owned()));
    // A newline, unless `text` is empty or already ends in one.
    let newline = |text: &Name, unless: &str| Expr::If {
        condition: Box::new(Expr::Matches {
            text: Box::new(var(text)),
            pattern: Pattern::try_from(unless.to_owned()).expect("a literal pattern"),
        }),
        then: Box::new(str("")),
        otherwise: Box::new(str("\n")),
    };
    let both = Expr::Concat(vec![
        var(&old_name),
        newline(&old_name, r"(?:\A|\n)\z"),
        var(&added_name),
        newline(&added_name, r"\n\z"),
    ]);
    Expr::Let {
        name: old.0,
        value: Box::new(old.1),
        body: Box::new(Expr::Let {
            name: added.0,
            value: Box::new(added.1),
            body: Box::new(Expr::If {
                condition: Box::new(Expr::Eq(Box::new(var(&added_name)), Box::new(str("")))),
                then: Box::new(var(&old_name)),
                otherwise: Box::new(both),
            }),
        }),
    }
}

/// A name the desugaring binds.
fn name(name: &str) -> Name {
    Name::try_from(name.to_owned()).expect("a literal name")
}

/// Fail with `message` unless the selection is not empty.
fn expect(selection: Expr, message: String) -> Stmt {
    Stmt::Assert {
        condition: defined(selection),
        message,
    }
}

/// `<element name="name">`, as an XML selector.
fn feature_match(element: &str, name: &str) -> XmlMatch {
    let local = |local: &str| XmlName {
        namespace: None,
        local: local.to_owned(),
    };
    XmlMatch {
        name: local(element),
        predicate: Some(XmlPredicate::Attribute {
            name: local("name"),
            value: name.to_owned(),
        }),
    }
}
