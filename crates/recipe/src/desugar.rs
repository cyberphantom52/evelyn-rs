//! Surface recipes (steps of operations, `let:` and `when:`) into the primitive tree.

use crate::{
    Error, Expr as Surface, Operation, PackageEdit, Recipe, Step,
    tree::{BinaryOp, Expr, Scope, Stmt, Transform, Tree, UnaryOp},
    value::Value,
};
use model::{DevicePath, RelativePath};

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
        }];
    }
    if let Some(condition) = &step.when {
        body = vec![Stmt::When {
            condition: expr(condition),
            body,
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
        } => Stmt::Copy {
            selection: select(
                pattern.as_ref(),
                Scope {
                    inside: Some(*from),
                    ..Scope::default()
                },
            ),
            to: to.clone(),
            metadata: *metadata,
        },
        Operation::Remove {
            from,
            partition,
            pattern,
        } => Stmt::Delete {
            selection: select(
                pattern,
                Scope {
                    origin: *from,
                    partition: partition.clone(),
                    ..Scope::default()
                },
            ),
            pattern: pattern.clone(),
        },
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
        Operation::TextPatch(path, patch) => Stmt::Edit {
            selection: select(path.as_ref(), Scope::default()),
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
            body: edits.iter().map(package_edit).collect(),
        },
        Operation::Append { target, content } => Stmt::Edit {
            selection: select(target.as_ref(), Scope::default()),
            transform: Transform::Append(content.clone()),
        },
        Operation::AssertHash(path, sha256) => Stmt::Assert {
            condition: Expr::Binary {
                op: BinaryOp::Eq,
                left: Box::new(Expr::Hash(Box::new(select(
                    path.as_ref(),
                    Scope::default(),
                )))),
                right: Box::new(Expr::Literal(Value::Str(sha256.to_string()))),
            },
            message: format!("{path} does not have SHA-256 {sha256}"),
        },
    };
    vec![stmt]
}

/// A package edit, as a statement inside the package.
fn package_edit(edit: &PackageEdit) -> Stmt {
    let at = |path: &RelativePath| format!("/{path}");
    let class = |class: &String, transform| Stmt::Edit {
        selection: Expr::Class(class.clone()),
        transform,
    };
    match edit {
        PackageEdit::StubMethod {
            class: name,
            method,
            returns,
        } => class(
            name,
            Transform::StubMethod {
                class: name.clone(),
                method: method.clone(),
                returns: *returns,
            },
        ),
        PackageEdit::ReplaceMethod {
            class: name,
            method,
            body,
        } => class(
            name,
            Transform::ReplaceMethod {
                class: name.clone(),
                method: method.clone(),
                body: body.clone(),
            },
        ),
        PackageEdit::InsertMethod {
            class: name,
            method,
            body,
        } => class(
            name,
            Transform::InsertMethod {
                class: name.clone(),
                method: method.clone(),
                body: body.clone(),
            },
        ),
        PackageEdit::TextPatch {
            path,
            patch,
            before,
        } => Stmt::Edit {
            selection: select(&at(path), Scope::default()),
            transform: Transform::TextPatch {
                patch: patch.clone(),
                before: before.clone(),
            },
        },
        PackageEdit::Xml {
            path,
            before,
            edits,
        } => Stmt::Edit {
            selection: select(&at(path), Scope::default()),
            transform: Transform::Xml {
                before: before.clone(),
                edits: edits.clone(),
            },
        },
        PackageEdit::Install { path, asset } => Stmt::Write {
            asset: asset.clone(),
            to: DevicePath::try_from(at(path)).expect("a relative path below / is a device path"),
        },
    }
}
