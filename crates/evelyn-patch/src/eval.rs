//! The value of every expression a recipe writes, against the build composed so
//! far. An operator given a value it is not defined for is an error.

use crate::{Composition, Error, compose::Bound, compose::Context};
use recipe::{
    Expr, Location,
    value::{Located, Value},
};

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
            Expr::Bool(value) => Value::Bool(*value),
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
            Expr::Hash(inner) => {
                let selection = eval(inner)?;
                let [entry] = selection.as_selection()? else {
                    return Err(Error::Unsupported("Hash needs exactly one file"));
                };
                Value::Str(self.sha256(entry, context)?.to_string())
            }
            Expr::Below(inner) => {
                let mut found = Vec::new();
                for entry in eval(inner)?.as_selection()? {
                    found.push(entry.clone());
                    found.extend(self.below(entry, context)?);
                }
                found.sort();
                found.dedup();
                Value::Selection(found)
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
            // `All` and `Any` stop at the first condition that decides them.
            Expr::All(conditions) => {
                for condition in conditions {
                    if !eval(condition)?.as_bool()? {
                        return Ok(Value::Bool(false));
                    }
                }
                Value::Bool(true)
            }
            Expr::Any(conditions) => {
                for condition in conditions {
                    if eval(condition)?.as_bool()? {
                        return Ok(Value::Bool(true));
                    }
                }
                Value::Bool(false)
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
            Expr::Eq(left, right) => eval(left)?.equals(&eval(right)?)?,
            Expr::In(inner, values) => {
                let value = eval(inner)?;
                for text in values {
                    let text = Value::Str(recipe::substitute(text, bound)?);
                    if value.equals(&text)?.as_bool()? {
                        return Ok(Value::Bool(true));
                    }
                }
                Value::Bool(false)
            }
            Expr::If {
                condition,
                then,
                otherwise,
            } => match eval(condition)?.as_bool()? {
                true => eval(then)?,
                false => eval(otherwise)?,
            },
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
            Expr::Trim(inner) => match eval(inner)? {
                Value::Str(text) => Value::Str(text.trim().to_owned()),
                other => other.as_str().map(|_| other.clone())?,
            },
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

    /// The SHA-256 of a selected file.
    pub(crate) fn sha256(
        &self,
        entry: &Located,
        context: &Context<'_>,
    ) -> Result<android_firmware::Sha256Digest, Error> {
        match entry.place {
            recipe::value::Place::Build => {
                let target = self
                    .targets
                    .get(&entry.partition)
                    .ok_or_else(|| Error::UnknownPartition(entry.partition.clone()))?;
                Ok(target
                    .store(context.images)
                    .sha256(&target.file(&entry.path)?)?)
            }
            _ => Ok(android_firmware::Sha256Digest::bytes(
                &self.content(entry, context)?,
            )),
        }
    }

    /// The entries below a selected one, in the build or a firmware.
    fn below(&self, entry: &Located, context: &Context<'_>) -> Result<Vec<Located>, Error> {
        use recipe::value::Place;
        let below: Vec<_> = match entry.place {
            Place::Build => crate::layout::below(
                &self
                    .targets
                    .get(&entry.partition)
                    .ok_or_else(|| Error::UnknownPartition(entry.partition.clone()))?
                    .tree,
                &entry.path,
            )
            .cloned()
            .collect(),
            Place::Firmware(firmware) => crate::layout::below(
                context.source(&crate::files::source_of(firmware, &entry.partition))?,
                &entry.path,
            )
            .cloned()
            .collect(),
            Place::Package | Place::Asset => {
                return Err(Error::Unsupported(
                    "Below selects in the build or a firmware",
                ));
            }
        };
        Ok(below
            .into_iter()
            .map(|path| Located {
                place: entry.place,
                partition: entry.partition.clone(),
                path,
            })
            .collect())
    }
}
