//! Surface recipes (steps of operations, `let:` and `when:`) into the primitive tree.

use crate::{
    Data, Error, Feature, Metadata, Operation, Recipe, Step, XmlEdit, XmlMatch, XmlName, XmlPath,
    XmlPredicate, escape_attribute,
    tree::{Assigned, Expr, MethodEdit, Pattern, Scope, Stmt, Transform, Tree},
    value::Value,
};
use android_firmware::{Firmware, Name};

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
            condition: condition.clone(),
            body,
            skip: format!("condition does not hold: {}", shown(condition)),
        }];
    }
    for (name, value) in step.bindings.iter().rev() {
        body = vec![Stmt::Let {
            name: name.clone(),
            value: value.clone(),
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
fn shown(expr: &Expr) -> String {
    match expr.literal_str() {
        Some(text) => text.to_owned(),
        None => ron::to_string(expr).unwrap_or_default(),
    }
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
        Operation::Copy {
            from,
            path,
            to,
            metadata,
        } => {
            let source = select_expr(
                path.clone(),
                Scope {
                    inside: Some(*from),
                    ..Scope::default()
                },
            );
            let found = expect(source.clone(), format!("{} is not in {from}", shown(path)));
            let copy = Stmt::Copy {
                selection: source,
                to: to.clone(),
            };
            // What the copy put in the build, by where it went.
            let placed = to.as_ref().unwrap_or(path);
            let defaults = || Stmt::SetMetadata {
                selection: Expr::Below(Box::new(select_expr(placed.clone(), Scope::default()))),
                pattern: shown(placed),
                metadata: Assigned::Defaults,
            };
            return match metadata {
                Metadata::Copy => vec![found, copy],
                Metadata::Default => vec![found, copy, defaults()],
                Metadata::Explicit { .. } => vec![
                    found,
                    copy,
                    defaults(),
                    Stmt::SetMetadata {
                        selection: select_expr(placed.clone(), Scope::default()),
                        pattern: shown(placed),
                        metadata: Assigned::Fields(overrides(operation)),
                    },
                ],
            };
        }
        Operation::Remove {
            from,
            partition,
            pattern,
        } => {
            let selection = select(
                pattern,
                Scope {
                    origin: *from,
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
            selection: select(pattern.as_ref(), Scope::default()),
            pattern: pattern.to_string(),
            metadata: Assigned::Fields(overrides(operation)),
        },
        // Where the key is defined, everywhere in the target's scope; or added
        // where a new one goes.
        Operation::SetProperty { key, value, target } => Stmt::Edit {
            selection: or_else(
                property_files(key, target),
                Expr::NewPropertyFile(target.clone()),
            ),
            transform: Transform::Property {
                key: key.clone(),
                value: Some(value.clone()),
            },
        },
        // Every definition, or a lower one would show; there must be one.
        Operation::RemoveProperty { key, target } => {
            return vec![
                expect(
                    property_files(key, target),
                    format!("property {key} is not defined there"),
                ),
                Stmt::Edit {
                    selection: property_files(key, target),
                    transform: Transform::Property {
                        key: key.clone(),
                        value: None,
                    },
                },
            ];
        }
        Operation::Diff { target, patch } => Stmt::Edit {
            selection: select(target.as_ref(), Scope::default()),
            transform: Transform::Diff(patch.clone()),
        },
        Operation::WriteBytes { target, edits } => Stmt::Edit {
            selection: select(target.as_ref(), Scope::default()),
            transform: Transform::WriteBytes(edits.clone()),
        },
        Operation::EditXml { target, edits } => Stmt::Edit {
            selection: select(target.as_ref(), Scope::default()),
            transform: Transform::Xml(edits.clone()),
        },
        Operation::Package { target, edits } => Stmt::Package {
            package: target.clone(),
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
        } => smali(class, method, MethodEdit::Replace(body.clone())),
        Operation::InsertMethod {
            class,
            method,
            body,
        } => smali(class, method, MethodEdit::Insert(body.clone())),
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
                    expect: None,
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
                    expect: None,
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
        Operation::AddPolicy { target, policies } => Stmt::Edit {
            selection: select(target.as_ref(), Scope::default()),
            transform: Transform::AddPolicy {
                policies: policies.clone(),
                resolve_with: Expr::PolicyFiles,
            },
        },
        Operation::Append { target, content } => Stmt::Edit {
            selection: select(target.as_ref(), Scope::default()),
            transform: Transform::Append(match content {
                Data::Text(text) => Expr::Literal(Value::Str(text.clone())),
                Data::File { from, path } => {
                    Expr::Read(Box::new(firmware_file(path.as_ref(), *from)))
                }
                // The lines that contain any of `containing` and none of `except`.
                Data::Lines {
                    from,
                    path,
                    containing,
                    except,
                } => {
                    let line = name("line");
                    let has = |any_of: &[String]| Expr::Matches {
                        text: Box::new(Expr::Var(line.clone())),
                        pattern: Pattern::try_from(
                            any_of
                                .iter()
                                .map(|text| regex::escape(text))
                                .collect::<Vec<_>>()
                                .join("|"),
                        )
                        .expect("escaped alternatives compile"),
                    };
                    let mut keep = vec![has(containing)];
                    if !except.is_empty() {
                        keep.push(Expr::Not(Box::new(has(except))));
                    }
                    let lines = Expr::Lines(Box::new(Expr::Read(Box::new(firmware_file(
                        path.as_ref(),
                        *from,
                    )))));
                    Expr::Join(Box::new(Expr::Filter {
                        over: Box::new(lines),
                        name: line,
                        keep: Box::new(Expr::All(keep)),
                    }))
                }
            }),
        },
        Operation::AssertHash { target, sha256 } => Stmt::Assert {
            condition: Expr::Eq(
                Box::new(Expr::Hash(Box::new(select(
                    target.as_ref(),
                    Scope::default(),
                )))),
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
        | Operation::Diff { target, .. }
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

/// The file a feature of its kind is declared in.
fn feature_file(feature: &Feature) -> &str {
    match feature {
        Feature::Android(_) => "/my_product/etc/permissions/com.oplus.android-features.xml",
        Feature::Oplus(_) => "/my_product/etc/permissions/oplus.feature.android.xml",
        Feature::App { file, .. } => file.as_ref(),
    }
}

/// The property files defining `key` in a target's scope.
fn property_files(key: &str, target: &Option<crate::PropertyTarget>) -> Expr {
    Expr::PropertyFiles {
        key: key.to_owned(),
        target: target.clone(),
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

/// What `pattern` (a device path) matches in a firmware, as a selection.
fn firmware_file(pattern: &str, from: Firmware) -> Expr {
    select(
        pattern,
        Scope {
            inside: Some(from.into()),
            ..Scope::default()
        },
    )
}

/// The metadata an operation that sets some sets.
fn overrides(operation: &Operation) -> crate::Overrides {
    operation
        .overrides()
        .expect("SetMetadata and explicit imports set metadata")
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
