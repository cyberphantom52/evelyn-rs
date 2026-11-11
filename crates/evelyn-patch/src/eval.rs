//! The value of every expression a recipe writes, against the build composed so
//! far. An operator given a value it is not defined for is an error.

use crate::{
    Composition, Error,
    compose::{Bound, Context},
};
use recipe::{Expr, Location, value::Value};

impl Composition {
    pub(crate) fn eval(
        &self,
        expr: &Expr,
        bound: &Bound,
        context: &Context<'_>,
    ) -> Result<Value, Error> {
        let eval = |expr: &Expr| self.eval(expr, bound, context);
        Ok(match expr {
            Expr::Text(written) => Value::Str(recipe::substitute(written, bound)?),
            Expr::At(location) => Value::Selection(self.select(location, bound, context)?),
            Expr::Read(inner) => {
                let selection = eval(inner)?;
                let [entry] = selection.as_selection()? else {
                    return Err(Error::Unsupported("Read needs exactly one file"));
                };
                Value::Str(
                    String::from_utf8(self.content(entry, context)?)
                        .map_err(|_| Error::Unsupported("Read reads UTF-8 text"))?,
                )
            }
            // The last assignment in the files, or the value the device ends up with.
            Expr::Property { key, target } => {
                let key = recipe::substitute(key, bound)?;
                let Some(target) = target else {
                    return Ok(self
                        .loaded(context)?
                        .device_property(&key)
                        .map_or(Value::Unset, Value::Str));
                };
                let mut value = Value::Unset;
                for file in self.select(target, bound, context)? {
                    let text = String::from_utf8_lossy(&self.content(&file, context)?).into_owned();
                    if let Some(assigned) = android_property::get(&text, &key) {
                        value = Value::Str(assigned.to_owned());
                    }
                }
                value
            }
            Expr::Exists(inner) => match &**inner {
                Expr::At(Location::Partition(name)) => Value::Bool(self.targets.contains_key(name)),
                inner => eval(inner)?.defined()?,
            },
            Expr::Not(inner) => (!eval(inner)?)?,
            // `All` stops at the first condition that does not hold.
            Expr::All(conditions) => {
                for condition in conditions {
                    if !eval(condition)?.as_bool()? {
                        return Ok(Value::Bool(false));
                    }
                }
                Value::Bool(true)
            }
            Expr::Union(selections) => {
                let mut union = Vec::new();
                for selection in selections {
                    union.extend_from_slice(eval(selection)?.as_selection()?);
                }
                let mut seen = std::collections::BTreeSet::new();
                union.retain(|entry| seen.insert(entry.clone()));
                Value::Selection(union)
            }
            // Each string of a list, or the device path of each entry of a selection.
            Expr::Filter { over, name, keep } => {
                let holds = |value: String| {
                    let mut bound = bound.clone();
                    bound.insert(name.clone(), value);
                    Ok::<_, Error>(self.eval(keep, &bound, context)?.as_bool()?)
                };
                match eval(over)? {
                    Value::List(items) => {
                        let mut kept = Vec::new();
                        for item in items {
                            if holds(item.clone())? {
                                kept.push(item);
                            }
                        }
                        Value::List(kept)
                    }
                    Value::Selection(entries) => {
                        let mut kept = Vec::new();
                        for entry in entries {
                            if holds(self.device_path(&entry, context))? {
                                kept.push(entry);
                            }
                        }
                        Value::Selection(kept)
                    }
                    other => {
                        return Err(recipe::value::ValueError {
                            op: "Filter",
                            found: other.kind().to_owned(),
                        }
                        .into());
                    }
                }
            }
            Expr::Lines(inner) => eval(inner)?.lines()?,
            Expr::Join(inner) => eval(inner)?.join()?,
            Expr::Contains(text, part) => {
                let part = recipe::substitute(part, bound)?;
                let text = eval(text)?;
                Value::Bool(text.as_str()?.is_some_and(|text| text.contains(&part)))
            }
            Expr::Matches(text, pattern) => {
                let text = eval(text)?;
                Value::Bool(text.as_str()?.is_some_and(|text| pattern.is_match(text)))
            }
        })
    }

    /// A string expression's value, which must be set.
    pub(crate) fn string(
        &self,
        expr: &Expr,
        bound: &Bound,
        context: &Context<'_>,
    ) -> Result<String, Error> {
        self.eval(expr, bound, context)?
            .as_str()?
            .map(str::to_owned)
            .ok_or_else(|| Error::Unset(expr.to_string()))
    }
}
