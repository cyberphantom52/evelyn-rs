//! Walking the primitive tree. Every pass over a recipe (the load-time check, the
//! partitions, sources and assets it needs) is a [`Visitor`], so the tree's shape
//! is matched in one place: [`Tree::walk`]. A visitor's `Invalid` errors are
//! reported with the step they are in.

use crate::{
    Error,
    tree::{BinaryOp, Expr, Stmt, Transform, Tree, UnaryOp},
    value::Value,
};
use android_firmware::Name;

pub trait Visitor {
    /// Called for every statement, with the names enclosing `Let`s bind.
    fn stmt(&mut self, _stmt: &Stmt, _bound: &[&Name]) -> Result<(), Error> {
        Ok(())
    }

    /// Called for every expression, nested ones included.
    fn expr(&mut self, _expr: &Expr, _bound: &[&Name]) -> Result<(), Error> {
        Ok(())
    }
}

impl Tree {
    pub fn walk(&self, visitor: &mut impl Visitor) -> Result<(), Error> {
        walk(&self.body, visitor)
    }
}

/// Walk statements on their own, such as a `Within`'s body.
pub fn walk(body: &[Stmt], visitor: &mut impl Visitor) -> Result<(), Error> {
    let mut bound = Vec::new();
    body.iter()
        .try_for_each(|stmt| walk_stmt(stmt, visitor, &mut bound))
}

fn walk_stmt<'a>(
    stmt: &'a Stmt,
    visitor: &mut impl Visitor,
    bound: &mut Vec<&'a Name>,
) -> Result<(), Error> {
    visitor.stmt(stmt, bound)?;
    match stmt {
        Stmt::Step { id, body } => body
            .iter()
            .try_for_each(|stmt| walk_stmt(stmt, visitor, bound))
            .map_err(|error| match error {
                Error::Invalid(message) => Error::Invalid(format!("step {id}: {message}")),
                error => error,
            }),
        Stmt::Within { body, .. } => body
            .iter()
            .try_for_each(|stmt| walk_stmt(stmt, visitor, bound)),
        Stmt::Let { name, value, body } => {
            walk_expr(value, visitor, bound)?;
            bound.push(name);
            let walked = body
                .iter()
                .try_for_each(|stmt| walk_stmt(stmt, visitor, bound));
            bound.pop();
            walked
        }
        Stmt::When {
            condition, body, ..
        } => {
            walk_expr(condition, visitor, bound)?;
            body.iter()
                .try_for_each(|stmt| walk_stmt(stmt, visitor, bound))
        }
        Stmt::Edit {
            selection,
            transform:
                Transform::Append(text)
                | Transform::Sepolicy {
                    resolve_with: text, ..
                },
        } => {
            walk_expr(selection, visitor, bound)?;
            walk_expr(text, visitor, bound)
        }
        Stmt::Delete { selection, .. }
        | Stmt::Copy { selection, .. }
        | Stmt::SetMetadata { selection, .. }
        | Stmt::Edit { selection, .. } => walk_expr(selection, visitor, bound),
        Stmt::Assert { condition, .. } => walk_expr(condition, visitor, bound),
    }
}

fn walk_expr(expr: &Expr, visitor: &mut impl Visitor, bound: &[&Name]) -> Result<(), Error> {
    visitor.expr(expr, bound)?;
    match expr {
        Expr::Hash(inner)
        | Expr::Content(inner)
        | Expr::Below(inner)
        | Expr::Lines { text: inner, .. }
        | Expr::Unary { expr: inner, .. } => walk_expr(inner, visitor, bound),
        Expr::Containing {
            selection: first,
            text: second,
        }
        | Expr::Let {
            value: first,
            body: second,
            ..
        }
        | Expr::Filter {
            over: first,
            keep: second,
            ..
        }
        | Expr::Binary {
            left: first,
            right: second,
            ..
        } => {
            walk_expr(first, visitor, bound)?;
            walk_expr(second, visitor, bound)
        }
        Expr::If {
            condition,
            then,
            otherwise,
        } => {
            walk_expr(condition, visitor, bound)?;
            walk_expr(then, visitor, bound)?;
            walk_expr(otherwise, visitor, bound)
        }
        Expr::Literal(_)
        | Expr::Var(_)
        | Expr::Select { .. }
        | Expr::Prop { .. }
        | Expr::PropertyFiles { .. }
        | Expr::PolicyFiles
        | Expr::NewPropertyFile(_)
        | Expr::Partition(_) => Ok(()),
    }
}

/// The kind of value an expression produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Str,
    Bool,
    List,
    Selection,
}

/// The load-time check: every expression gets operands of the kinds its operator
/// is defined for, every name it uses is bound, and conditions are booleans.
pub(crate) struct Check;

fn expect(found: Kind, allowed: &[Kind], what: &str) -> Result<(), Error> {
    if allowed.contains(&found) {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "{what} takes {allowed:?}, not {found:?}"
        )))
    }
}

/// The names bound around an expression, innermost last, with their kinds.
type Bound<'a> = Vec<(&'a Name, Kind)>;

/// The kind `expr` produces, once its operands are checked to be of the kinds its
/// operator takes.
fn kind<'a>(expr: &'a Expr, bound: &Bound<'a>) -> Result<Kind, Error> {
    let of = |inner: &'a Expr, allowed: &[Kind], what: &str| {
        let found = kind(inner, bound)?;
        expect(found, allowed, what).map(|()| found)
    };
    Ok(match expr {
        Expr::Literal(Value::Str(_) | Value::Unset) | Expr::Prop { .. } => Kind::Str,
        Expr::Literal(Value::Bool(_)) | Expr::Partition(_) => Kind::Bool,
        Expr::Literal(Value::List(_)) => Kind::List,
        Expr::Literal(Value::Selection(_))
        | Expr::Select { .. }
        | Expr::PropertyFiles { .. }
        | Expr::PolicyFiles
        | Expr::NewPropertyFile(_) => Kind::Selection,
        Expr::Var(name) => match bound.iter().rev().find(|(bound, _)| *bound == name) {
            Some((_, kind)) => *kind,
            None => {
                return Err(Error::Invalid(format!(
                    "{name} is not bound by an enclosing Let or Filter"
                )));
            }
        },
        Expr::Hash(inner) => of(inner, &[Kind::Selection], "Hash").map(|_| Kind::Str)?,
        Expr::Content(inner) => of(inner, &[Kind::Selection], "Content").map(|_| Kind::Str)?,
        Expr::Lines { text, .. } => of(text, &[Kind::Str], "Lines").map(|_| Kind::Str)?,
        Expr::Below(inner) => of(inner, &[Kind::Selection], "Below")?,
        Expr::Containing { selection, text } => {
            of(text, &[Kind::Str], "Containing")?;
            of(selection, &[Kind::Selection], "Containing")?
        }
        Expr::Let { name, value, body } => kind(body, &binding(bound, name, kind(value, bound)?))?,
        Expr::Filter { over, name, keep } => {
            let over = of(over, &[Kind::List, Kind::Selection], "Filter")?;
            let each = if over == Kind::List { Kind::Str } else { over };
            expect(
                kind(keep, &binding(bound, name, each))?,
                &[Kind::Bool],
                "Filter",
            )?;
            over
        }
        Expr::If {
            condition,
            then,
            otherwise,
        } => {
            of(condition, &[Kind::Bool], "If")?;
            let then = kind(then, bound)?;
            of(otherwise, &[then], "If")?
        }
        Expr::Unary { op, expr } => match op {
            UnaryOp::Not => of(expr, &[Kind::Bool], "Not")?,
            UnaryOp::Defined => {
                of(expr, &[Kind::Str, Kind::Selection], "Defined").map(|_| Kind::Bool)?
            }
        },
        Expr::Binary { op, left, right } => match op {
            BinaryOp::And => {
                of(left, &[Kind::Bool], "All")?;
                of(right, &[Kind::Bool], "All")?
            }
            // Of booleans, either holds; of selections, what either selects.
            BinaryOp::Or => {
                let left = of(left, &[Kind::Bool, Kind::Selection], "Any")?;
                of(right, &[left], "Any")?
            }
            BinaryOp::Eq => {
                let left = of(left, &[Kind::Str, Kind::Bool], "Eq")?;
                of(right, &[left], "Eq").map(|_| Kind::Bool)?
            }
            BinaryOp::In => {
                of(left, &[Kind::Str], "In")?;
                of(right, &[Kind::List], "In").map(|_| Kind::Bool)?
            }
        },
    })
}

/// `bound`, and `name` bound to a value of kind `kind`.
fn binding<'a>(bound: &Bound<'a>, name: &'a Name, kind: Kind) -> Bound<'a> {
    let mut bound = bound.clone();
    bound.push((name, kind));
    bound
}

impl Visitor for Check {
    fn stmt(&mut self, stmt: &Stmt, bound: &[&Name]) -> Result<(), Error> {
        let of = |expr: &Expr, allowed: &[Kind], what: &str| {
            expect(kind(expr, &Vec::new())?, allowed, what)
        };
        match stmt {
            // Its body is checked as it is walked.
            Stmt::Step { .. } => return Ok(()),
            Stmt::Let { value, .. } => of(value, &[Kind::Str], "let:")?,
            Stmt::When { condition, .. } | Stmt::Assert { condition, .. } => {
                of(condition, &[Kind::Bool], "a condition")?
            }
            Stmt::Edit {
                selection,
                transform:
                    Transform::Append(text)
                    | Transform::Sepolicy {
                        resolve_with: text, ..
                    },
            } => {
                of(selection, &[Kind::Selection], "a selection")?;
                match &stmt {
                    Stmt::Edit {
                        transform: Transform::Append(_),
                        ..
                    } => of(text, &[Kind::Str], "Append")?,
                    _ => of(text, &[Kind::Selection], "SepolicyPatch")?,
                }
            }
            Stmt::Delete { selection, .. }
            | Stmt::Copy { selection, .. }
            | Stmt::SetMetadata { selection, .. }
            | Stmt::Edit { selection, .. } => of(selection, &[Kind::Selection], "a selection")?,
            Stmt::Within { .. } => {}
        }
        // Only the statement's own fields: bodies are checked as they are walked.
        let own = match stmt {
            Stmt::Let { value, .. } => ron(value)?,
            Stmt::When { condition, .. } => ron(condition)?,
            Stmt::Within {
                package, before, ..
            } => ron(&(package, before))?,
            other => ron(other)?,
        };
        match placeholders(&own)
            .into_iter()
            .find(|name| !bound.iter().any(|bound| bound.as_ref() == *name))
        {
            Some(unbound) => Err(Error::Invalid(format!(
                "{{{unbound}}} is not bound by an enclosing let:"
            ))),
            None => Ok(()),
        }
    }
}

fn ron(value: &impl serde::Serialize) -> Result<String, Error> {
    Ok(crate::ron_options().to_string(value)?)
}

/// The `{name}` placeholders in `text`. Names are `[A-Za-z0-9_-]`, so a
/// `{namespace-uri}` in an XML path, which has `:` and `/`, is not one.
fn placeholders(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        rest = &rest[start + 1..];
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .unwrap_or(rest.len());
        if end > 0 && rest[end..].starts_with('}') {
            found.push(&rest[..end]);
        }
    }
    found
}
