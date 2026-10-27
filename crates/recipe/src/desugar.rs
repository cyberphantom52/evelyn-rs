//! Surface recipes (steps of operations, `let:` and `when:`) into the primitive tree.

use crate::{
    Data, Error, Expr as Surface, Feature, Metadata, Operation, Recipe, Step, XmlEdit, XmlMatch,
    XmlName, XmlPath, XmlPredicate,
    tree::{BinaryOp, Expr, Scope, Stmt, Transform, Tree, UnaryOp},
    value::Value,
};

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
                Some(Expr::Asset(asset.clone()))
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
                        inside: Some(*from),
                        ..Scope::default()
                    },
                ),
                to: to.clone(),
            };
            // What the copy put in the build, by where it went.
            let placed = to.as_ref().unwrap_or(pattern).to_string();
            let reset = || Stmt::Reset {
                selection: select(&placed, Scope::default()),
            };
            let found = expect(
                select(
                    pattern.as_ref(),
                    Scope {
                        inside: Some(*from),
                        ..Scope::default()
                    },
                ),
                format!("{pattern} is not in the {} firmware", from.name()),
            );
            return match metadata {
                Metadata::Copy => vec![found, copy],
                Metadata::Default => vec![found, copy, reset()],
                Metadata::Explicit {
                    uid,
                    gid,
                    mode,
                    selinux,
                } => vec![
                    found,
                    copy,
                    reset(),
                    Stmt::SetMeta {
                        selection: select(&placed, Scope::default()),
                        pattern: placed.clone(),
                        overrides: crate::Overrides {
                            uid: *uid,
                            gid: *gid,
                            mode: *mode,
                            selinux: selinux.clone(),
                        },
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
        Operation::Relocate { from, to } => Stmt::Move {
            from: from.clone(),
            to: to.clone(),
        },
        Operation::Install { asset, to } => Stmt::Write {
            asset: asset.clone(),
            to: to.clone(),
        },
        Operation::SetMetadata {
            pattern,
            uid,
            gid,
            mode,
            selinux,
        } => Stmt::SetMeta {
            selection: select(pattern.as_ref(), Scope::default()),
            pattern: pattern.to_string(),
            overrides: crate::Overrides {
                uid: *uid,
                gid: *gid,
                mode: *mode,
                selinux: selinux.clone(),
            },
        },
        Operation::SetProperty { key, value, target } => Stmt::Edit {
            selection: Expr::PropFiles {
                key: key.clone(),
                target: target.clone(),
                create: true,
                every: target.is_none(),
            },
            transform: Transform::SetProp {
                key: key.clone(),
                value: value.clone(),
            },
        },
        // A device-wide removal clears every definition, or a lower one would show.
        Operation::RemoveProperty { key, target } => Stmt::Edit {
            selection: Expr::PropFiles {
                key: key.clone(),
                target: target.clone(),
                create: false,
                every: target.is_none(),
            },
            transform: Transform::DelProp { key: key.clone() },
        },
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
        } => Stmt::Edit {
            selection: Expr::Class(class.clone()),
            transform: Transform::StubMethod {
                class: class.clone(),
                method: method.clone(),
                returns: *returns,
            },
        },
        Operation::ReplaceMethod {
            class,
            method,
            body,
        } => Stmt::Edit {
            selection: Expr::Class(class.clone()),
            transform: Transform::ReplaceMethod {
                class: class.clone(),
                method: method.clone(),
                body: body.clone(),
            },
        },
        Operation::InsertMethod {
            class,
            method,
            body,
        } => Stmt::Edit {
            selection: Expr::Class(class.clone()),
            transform: Transform::InsertMethod {
                class: class.clone(),
                method: method.clone(),
                body: body.clone(),
            },
        },
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
                .map(|element| XmlEdit::RemoveAll {
                    element: feature_match(element, name),
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
        Operation::Append { target, content } => Stmt::Edit {
            selection: select(target.as_ref(), Scope::default()),
            transform: Transform::Append(match content {
                Data::Text(text) => Expr::Literal(Value::Str(text.clone())),
                Data::File { from, path } => Expr::Content(Box::new(select(
                    path.as_ref(),
                    Scope {
                        inside: Some(*from),
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
    // An operation on one named file needs it there.
    let target = match operation {
        Operation::SetMetadata {
            pattern: target, ..
        }
        | Operation::TextPatch { target, .. }
        | Operation::BinaryPatch { target, .. }
        | Operation::Xml { target, .. }
        | Operation::Append { target, .. } => target.to_string(),
        Operation::EnableFeature(feature) => feature_file(feature).to_owned(),
        _ => return vec![stmt],
    };
    vec![
        expect(
            select(&target, Scope::default()),
            format!("{target} is not there"),
        ),
        stmt,
    ]
}

/// The file a feature of its kind is declared in.
fn feature_file(feature: &Feature) -> &str {
    match feature {
        Feature::Android(_) => "/my_product/etc/permissions/com.oplus.android-features.xml",
        Feature::Oplus(_) => "/my_product/etc/permissions/oplus.feature.android.xml",
        Feature::App { file, .. } => file.as_ref(),
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
