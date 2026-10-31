//! Surface recipes (steps of operations, `let:` and `when:`) into the primitive tree.

use crate::{
    Data, Error, Expr as Surface, Feature, Metadata, Operation, Recipe, Step, XmlEdit, XmlMatch,
    XmlName, XmlPath, XmlPredicate,
    tree::{Assigned, BinaryOp, Expr, MethodEdit, Scope, Source, Stmt, Transform, Tree, UnaryOp},
    value::Value,
};
use android_firmware::RelativePath;

impl Recipe {
    /// The recipe as primitives, its steps in dependency order.
    pub fn tree(&self) -> Result<Tree, Error> {
        Ok(Tree {
            name: self.name.clone(),
            body: self.ordered_steps()?.into_iter().map(step).collect(),
        })
    }
}

/// `let:` bindings enclose `when:`, which encloses the operations; a templated
/// asset that is missing skips the step like an unset binding.
fn step(step: &Step) -> Stmt {
    let mut body: Vec<Stmt> = step.operations.iter().flat_map(operation).collect();
    let assets: Vec<Expr> = step
        .operations
        .iter()
        .filter_map(|operation| match operation {
            Operation::Install { asset, .. } if asset.as_ref().contains('{') => {
                Some(defined(asset_file(asset)))
            }
            _ => None,
        })
        .collect();
    if !assets.is_empty() {
        body = vec![Stmt::When {
            condition: all(assets),
            body,
            skip: "a templated asset is not there".into(),
        }];
    }
    if let Some(condition) = &step.when {
        body = vec![Stmt::When {
            condition: expr(condition),
            body,
            skip: format!(
                "condition does not hold: {}",
                ron::to_string(condition).unwrap_or_default()
            ),
        }];
    }
    for (name, value) in step.bindings.iter().rev() {
        body = vec![Stmt::Let {
            name: name.clone(),
            value: expr(value),
            body,
        }];
    }
    Stmt::Step {
        id: step.id.clone(),
        body,
    }
}

pub(crate) fn expr(surface: &Surface) -> Expr {
    let unary = |op, inner: &Surface| Expr::Unary {
        op,
        expr: Box::new(expr(inner)),
    };
    let binary = |op, left: Expr, right: Expr| Expr::Binary {
        op,
        left: Box::new(left),
        right: Box::new(right),
    };
    match surface {
        Surface::GetProperty { key, target } => Expr::Prop {
            key: key.clone(),
            target: target.clone(),
        },
        Surface::Defined(inner) => unary(UnaryOp::Defined, inner),
        Surface::Eq(inner, value) => binary(
            BinaryOp::Eq,
            expr(inner),
            Expr::Literal(Value::Str(value.clone())),
        ),
        Surface::In(inner, values) => binary(
            BinaryOp::In,
            expr(inner),
            Expr::Literal(Value::List(values.clone())),
        ),
        Surface::Exists(path) => Expr::Unary {
            op: UnaryOp::Defined,
            expr: Box::new(select(path.as_ref(), Scope::default())),
        },
        Surface::Partition(name) => Expr::Partition(name.clone()),
        Surface::Not(inner) => unary(UnaryOp::Not, inner),
        Surface::All(all_of) => all(all_of.iter().map(expr).collect()),
        Surface::Any(any_of) => any_of
            .iter()
            .map(expr)
            .reduce(|left, right| binary(BinaryOp::Or, left, right))
            .unwrap_or(Expr::Literal(Value::Bool(false))),
    }
}

fn all(exprs: Vec<Expr>) -> Expr {
    exprs
        .into_iter()
        .reduce(|left, right| Expr::Binary {
            op: BinaryOp::And,
            left: Box::new(left),
            right: Box::new(right),
        })
        .unwrap_or(Expr::Literal(Value::Bool(true)))
}

fn select(pattern: &str, scope: Scope) -> Expr {
    Expr::Select {
        pattern: pattern.to_owned(),
        scope,
    }
}

fn operation(operation: &Operation) -> Vec<Stmt> {
    let stmt = match operation {
        Operation::Import {
            from,
            pattern,
            to,
            metadata,
        } => {
            let copy = Stmt::Copy {
                selection: select(
                    pattern.as_ref(),
                    Scope {
                        inside: Some(Source::Firmware(*from)),
                        ..Scope::default()
                    },
                ),
                to: to.clone(),
            };
            // What the copy put in the build, by where it went.
            let placed = to.as_ref().unwrap_or(pattern).to_string();
            let defaults = || Stmt::SetMetadata {
                selection: Expr::Below(Box::new(select(&placed, Scope::default()))),
                pattern: placed.clone(),
                metadata: Assigned::Defaults,
            };
            let found = expect(
                select(
                    pattern.as_ref(),
                    Scope {
                        inside: Some(Source::Firmware(*from)),
                        ..Scope::default()
                    },
                ),
                format!("{pattern} is not in the {from} firmware"),
            );
            return match metadata {
                Metadata::Copy => vec![found, copy],
                Metadata::Default => vec![found, copy, defaults()],
                Metadata::Explicit {
                    uid,
                    gid,
                    mode,
                    selinux,
                } => vec![
                    found,
                    copy,
                    defaults(),
                    Stmt::SetMetadata {
                        selection: select(&placed, Scope::default()),
                        pattern: placed.clone(),
                        metadata: Assigned::Fields(crate::Overrides {
                            uid: *uid,
                            gid: *gid,
                            mode: *mode,
                            selinux: selinux.clone(),
                        }),
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
                body: vec![Stmt::Delete { selection }],
                skip: format!("nothing to remove: {pattern}"),
            }
        }
        // Copying within the build carries content and metadata; what was copied
        // is then removed where it was.
        Operation::Relocate { from, to } => {
            let from = select(from.as_ref(), Scope::default());
            return vec![
                expect(from.clone(), format!("{} is not there", ron_of(&from))),
                Stmt::Copy {
                    selection: from.clone(),
                    to: Some(to.clone()),
                },
                Stmt::Delete { selection: from },
            ];
        }
        Operation::Install { asset, to } => Stmt::Copy {
            selection: asset_file(asset),
            to: Some(to.clone()),
        },
        Operation::SetMetadata {
            pattern,
            uid,
            gid,
            mode,
            selinux,
        } => Stmt::SetMetadata {
            selection: select(pattern.as_ref(), Scope::default()),
            pattern: pattern.to_string(),
            metadata: Assigned::Fields(crate::Overrides {
                uid: *uid,
                gid: *gid,
                mode: *mode,
                selinux: selinux.clone(),
            }),
        },
        // Where the key is defined, everywhere in the target's scope; or added
        // where a new one goes.
        Operation::SetProperty { key, value, target } => Stmt::Edit {
            selection: Expr::Else(
                Box::new(property_files(key, target)),
                Box::new(Expr::NewPropertyFile(target.clone())),
            ),
            transform: Transform::Prop {
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
                    transform: Transform::Prop {
                        key: key.clone(),
                        value: None,
                    },
                },
            ];
        }
        Operation::TextPatch { target, patch } => Stmt::Edit {
            selection: select(target.as_ref(), Scope::default()),
            transform: Transform::TextPatch {
                patch: patch.clone(),
                before: None,
            },
        },
        Operation::BinaryPatch {
            target,
            before,
            edits,
        } => Stmt::Edit {
            selection: select(target.as_ref(), Scope::default()),
            transform: Transform::Bytes {
                before: before.clone(),
                edits: edits.clone(),
            },
        },
        Operation::Xml {
            target,
            before,
            edits,
        } => Stmt::Edit {
            selection: select(target.as_ref(), Scope::default()),
            transform: Transform::Xml {
                before: before.clone(),
                edits: edits.clone(),
            },
        },
        Operation::Package {
            target,
            before,
            edits,
        } => Stmt::Within {
            package: target.clone(),
            before: before.clone(),
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
            let args = args.map_or(String::new(), |args| format!(" args=\"{}\"", escape(args)));
            Stmt::Edit {
                selection: select(file, Scope::default()),
                transform: Transform::Xml {
                    before: None,
                    edits: vec![XmlEdit::PutChild {
                        parent: XmlPath::try_from(root.to_owned()).expect("a literal XML path"),
                        key: feature_match(element, name),
                        expect: None,
                        element: format!("<{element} name=\"{}\"{args}/>", escape(name)),
                    }],
                },
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
            let files = Expr::Containing {
                selection: Box::new(Expr::Binary {
                    op: BinaryOp::Or,
                    left: Box::new(select("/*/etc/permissions/*.xml", Scope::default())),
                    right: Box::new(select("/*/etc/extension/*.xml", Scope::default())),
                }),
                text: Box::new(Expr::Literal(Value::Str(name.clone()))),
            };
            Stmt::When {
                condition: defined(files.clone()),
                body: vec![Stmt::Edit {
                    selection: files,
                    transform: Transform::Xml {
                        before: None,
                        edits,
                    },
                }],
                skip: format!("feature {name} is not declared"),
            }
        }
        Operation::SepolicyPatch { target, policies } => Stmt::Edit {
            selection: select(target.as_ref(), Scope::default()),
            transform: Transform::Sepolicy {
                policies: policies.clone(),
                resolve_with: Expr::PolicyFiles,
            },
        },
        Operation::Append { target, content } => Stmt::Edit {
            selection: select(target.as_ref(), Scope::default()),
            transform: Transform::Append(match content {
                Data::Text(text) => Expr::Literal(Value::Str(text.clone())),
                Data::File { from, path } => Expr::Content(Box::new(select(
                    path.as_ref(),
                    Scope {
                        inside: Some(Source::Firmware(*from)),
                        ..Scope::default()
                    },
                ))),
            }),
        },
        Operation::AssertHash { target, sha256 } => Stmt::Assert {
            condition: Expr::Binary {
                op: BinaryOp::Eq,
                left: Box::new(Expr::Hash(Box::new(select(
                    target.as_ref(),
                    Scope::default(),
                )))),
                right: Box::new(Expr::Literal(Value::Str(sha256.to_string()))),
            },
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
        | Operation::TextPatch { target, .. }
        | Operation::BinaryPatch { target, .. }
        | Operation::Xml { target, .. }
        | Operation::Append { target, .. }
        | Operation::SepolicyPatch { target, .. } => named(target.to_string()),
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

/// A recipe asset, as a selection.
fn asset_file(asset: &RelativePath) -> Expr {
    select(
        asset.as_ref(),
        Scope {
            inside: Some(Source::Assets),
            ..Scope::default()
        },
    )
}

/// The pattern a selection was written with, for messages.
fn ron_of(selection: &Expr) -> String {
    match selection {
        Expr::Select { pattern, .. } => pattern.clone(),
        other => ron::to_string(other).unwrap_or_default(),
    }
}

/// The selection is not empty.
fn defined(selection: Expr) -> Expr {
    Expr::Unary {
        op: UnaryOp::Defined,
        expr: Box::new(selection),
    }
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

/// Text as an XML attribute value.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
}
