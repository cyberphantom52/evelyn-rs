//! Walking the primitive tree. Every pass over a recipe (the load-time check, the
//! partitions, sources and assets it needs) is a [`Visitor`], so the tree's shape
//! is matched in one place: [`Tree::walk`]. A visitor's `Invalid` errors are
//! reported with the step they are in.

use crate::{
    Error,
    tree::{Expr, Stmt, Transform, Tree},
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

/// Walk statements on their own, such as a `Package`'s body.
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
        Stmt::Package { body, .. } => body
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
                Transform::Write(text)
                | Transform::AddPolicy {
                    resolve_with: text, ..
                },
        } => {
            walk_expr(selection, visitor, bound)?;
            walk_expr(text, visitor, bound)
        }
        Stmt::Copy { selection, to } => {
            walk_expr(selection, visitor, bound)?;
            to.iter().try_for_each(|to| walk_expr(to, visitor, bound))
        }
        Stmt::Remove { selection, .. }
        | Stmt::SetMetadata { selection, .. }
        | Stmt::Edit { selection, .. } => walk_expr(selection, visitor, bound),
        Stmt::Assert { condition, .. } => walk_expr(condition, visitor, bound),
    }
}

fn walk_expr(expr: &Expr, visitor: &mut impl Visitor, bound: &[&Name]) -> Result<(), Error> {
    visitor.expr(expr, bound)?;
    match expr {
        Expr::Hash(inner)
        | Expr::Read(inner)
        | Expr::Below(inner)
        | Expr::Not(inner)
        | Expr::Exists(inner)
        | Expr::Lines(inner)
        | Expr::Join(inner)
        | Expr::Matches { text: inner, .. }
        | Expr::Select { pattern: inner, .. } => walk_expr(inner, visitor, bound),
        Expr::All(exprs) | Expr::Any(exprs) | Expr::Union(exprs) | Expr::Concat(exprs) => exprs
            .iter()
            .try_for_each(|expr| walk_expr(expr, visitor, bound)),
        Expr::Let {
            value: first,
            body: second,
            ..
        }
        | Expr::Filter {
            over: first,
            keep: second,
            ..
        }
        | Expr::Eq(first, second)
        | Expr::In(first, second) => {
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
        | Expr::Property { .. }
        | Expr::PropertyFiles { .. }
        | Expr::PolicyFiles
        | Expr::NewPropertyFile(_)
        | Expr::HasPartition(_) => Ok(()),
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
        Expr::Literal(Value::Str(_) | Value::Unset) | Expr::Property { .. } => Kind::Str,
        Expr::Literal(Value::Bool(_)) | Expr::HasPartition(_) => Kind::Bool,
        Expr::Literal(Value::List(_)) => Kind::List,
        Expr::Select { pattern, .. } => {
            of(pattern, &[Kind::Str], "Select").map(|_| Kind::Selection)?
        }
        Expr::Concat(parts) => {
            for part in parts {
                of(part, &[Kind::Str], "Concat")?;
            }
            Kind::Str
        }
        Expr::Literal(Value::Selection(_))
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
        Expr::Read(inner) => of(inner, &[Kind::Selection], "Read").map(|_| Kind::Str)?,
        Expr::Below(inner) => of(inner, &[Kind::Selection], "Below")?,
        Expr::Matches { text, .. } => of(text, &[Kind::Str], "Matches").map(|_| Kind::Bool)?,
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
        Expr::Not(inner) => of(inner, &[Kind::Bool], "Not")?,
        Expr::Exists(inner) => {
            of(inner, &[Kind::Str, Kind::Selection], "Exists").map(|_| Kind::Bool)?
        }
        Expr::Lines(inner) => of(inner, &[Kind::Str], "Lines").map(|_| Kind::List)?,
        Expr::Join(inner) => of(inner, &[Kind::List], "Join").map(|_| Kind::Str)?,
        Expr::All(exprs) | Expr::Any(exprs) | Expr::Union(exprs) => {
            let (each, what) = match expr {
                Expr::All(_) => (Kind::Bool, "All"),
                Expr::Any(_) => (Kind::Bool, "Any"),
                _ => (Kind::Selection, "Union"),
            };
            for expr in exprs {
                of(expr, &[each], what)?;
            }
            each
        }
        Expr::Eq(left, right) => {
            let left = of(left, &[Kind::Str, Kind::Bool], "Eq")?;
            of(right, &[left], "Eq").map(|_| Kind::Bool)?
        }
        Expr::In(left, right) => {
            of(left, &[Kind::Str], "In")?;
            of(right, &[Kind::List], "In").map(|_| Kind::Bool)?
        }
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
        // A step's `let:` binds strings.
        let bound: Bound = bound.iter().map(|name| (*name, Kind::Str)).collect();
        let of =
            |expr: &Expr, allowed: &[Kind], what: &str| expect(kind(expr, &bound)?, allowed, what);
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
                    Transform::Write(text)
                    | Transform::AddPolicy {
                        resolve_with: text, ..
                    },
            } => {
                of(selection, &[Kind::Selection], "a selection")?;
                match &stmt {
                    Stmt::Edit {
                        transform: Transform::Write(_),
                        ..
                    } => of(text, &[Kind::Str], "Write")?,
                    _ => of(text, &[Kind::Selection], "AddPolicy")?,
                }
            }
            Stmt::Copy { selection, to } => {
                of(selection, &[Kind::Selection], "a selection")?;
                if let Some(to) = to {
                    of(to, &[Kind::Str], "a copy's to")?;
                }
            }
            Stmt::Remove { selection, .. }
            | Stmt::SetMetadata { selection, .. }
            | Stmt::Edit { selection, .. } => of(selection, &[Kind::Selection], "a selection")?,
            Stmt::Package { .. } => {}
        }
        Ok(())
    }
}
