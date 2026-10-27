//! Walking the primitive tree. Every pass over a recipe (the load-time check, the
//! partitions, sources and assets it needs) is a [`Visitor`], so the tree's shape
//! is matched in one place: [`Tree::walk`].

use crate::{
    Error,
    tree::{BinaryOp, Expr, Stmt, Tree, UnaryOp},
    value::Value,
};
use model::Name;

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
        Stmt::Step { body, .. } => body
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
        Stmt::When { condition, body } => {
            walk_expr(condition, visitor, bound)?;
            body.iter()
                .try_for_each(|stmt| walk_stmt(stmt, visitor, bound))
        }
        Stmt::Delete { selection, .. }
        | Stmt::Copy { selection, .. }
        | Stmt::Edit { selection, .. } => walk_expr(selection, visitor, bound),
        Stmt::Assert { condition, .. } => walk_expr(condition, visitor, bound),
        Stmt::Move { .. } | Stmt::Write { .. } | Stmt::WriteTree { .. } => Ok(()),
    }
}

fn walk_expr(expr: &Expr, visitor: &mut impl Visitor, bound: &[&Name]) -> Result<(), Error> {
    visitor.expr(expr, bound)?;
    match expr {
        Expr::Hash(inner) | Expr::Unary { expr: inner, .. } => walk_expr(inner, visitor, bound),
        Expr::Binary { left, right, .. } => {
            walk_expr(left, visitor, bound)?;
            walk_expr(right, visitor, bound)
        }
        Expr::Literal(_)
        | Expr::Select { .. }
        | Expr::Prop { .. }
        | Expr::PropFiles { .. }
        | Expr::Partition(_)
        | Expr::Asset(_) => Ok(()),
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

impl Expr {
    pub fn kind(&self) -> Kind {
        match self {
            Self::Literal(Value::Str(_) | Value::Unset) | Self::Prop { .. } | Self::Hash(_) => {
                Kind::Str
            }
            Self::Literal(Value::List(_)) => Kind::List,
            Self::Literal(Value::Selection(_)) | Self::Select { .. } | Self::PropFiles { .. } => {
                Kind::Selection
            }
            Self::Literal(Value::Bool(_))
            | Self::Partition(_)
            | Self::Asset(_)
            | Self::Unary { .. }
            | Self::Binary { .. } => Kind::Bool,
        }
    }
}

/// The load-time check: every expression gets operands of the kinds its operator
/// is defined for, `let:` binds strings, conditions are booleans, and every
/// `{name}` a statement uses is bound by an enclosing `let:`.
#[derive(Default)]
pub(crate) struct Check {
    step: Option<Name>,
}

fn expect(found: Kind, allowed: &[Kind], what: &str) -> Result<(), Error> {
    if allowed.contains(&found) {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "{what} takes {allowed:?}, not {found:?}"
        )))
    }
}

impl Visitor for Check {
    fn stmt(&mut self, stmt: &Stmt, bound: &[&Name]) -> Result<(), Error> {
        let step = |error: Error, step: &Option<Name>| match (error, step) {
            (Error::Invalid(message), Some(step)) => {
                Error::Invalid(format!("step {step}: {message}"))
            }
            (error, _) => error,
        };
        self.check(stmt, bound)
            .map_err(|error| step(error, &self.step))
    }

    fn expr(&mut self, expr: &Expr, _bound: &[&Name]) -> Result<(), Error> {
        let checked = check_expr(expr);
        checked.map_err(|error| match (error, &self.step) {
            (Error::Invalid(message), Some(step)) => {
                Error::Invalid(format!("step {step}: {message}"))
            }
            (error, _) => error,
        })
    }
}

impl Check {
    fn check(&mut self, stmt: &Stmt, bound: &[&Name]) -> Result<(), Error> {
        match stmt {
            Stmt::Step { id, .. } => {
                self.step = Some(id.clone());
                return Ok(());
            }
            Stmt::Let { value, .. } => expect(value.kind(), &[Kind::Str], "let:")?,
            Stmt::When { condition, .. } | Stmt::Assert { condition, .. } => {
                expect(condition.kind(), &[Kind::Bool], "a condition")?
            }
            Stmt::Delete { selection, .. }
            | Stmt::Copy { selection, .. }
            | Stmt::Edit { selection, .. } => {
                expect(selection.kind(), &[Kind::Selection], "a selection")?
            }
            _ => {}
        }
        // Only the statement's own fields: bodies are checked as they are walked.
        let own = match stmt {
            Stmt::Let { value, .. } => ron(value)?,
            Stmt::When { condition, .. } => ron(condition)?,
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

fn check_expr(expr: &Expr) -> Result<(), Error> {
    {
        match expr {
            Expr::Hash(inner) => expect(inner.kind(), &[Kind::Selection], "Hash"),
            Expr::Unary { op, expr } => match op {
                UnaryOp::Not => expect(expr.kind(), &[Kind::Bool], "Not"),
                UnaryOp::Defined => expect(expr.kind(), &[Kind::Str, Kind::Selection], "Defined"),
            },
            Expr::Binary { op, left, right } => {
                let (left, right) = (left.kind(), right.kind());
                match op {
                    BinaryOp::And | BinaryOp::Or => {
                        expect(left, &[Kind::Bool], "All/Any")?;
                        expect(right, &[Kind::Bool], "All/Any")
                    }
                    BinaryOp::Eq => {
                        expect(left, &[Kind::Str, Kind::Bool], "Eq")?;
                        expect(right, &[left], "Eq")
                    }
                    BinaryOp::In => {
                        expect(left, &[Kind::Str], "In")?;
                        expect(right, &[Kind::List], "In")
                    }
                }
            }
            _ => Ok(()),
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
