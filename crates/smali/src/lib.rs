//! Smali parsing and method-level editing.
//!
//! [`parse`] turns a `.smali` file into a [`Class`] whose nodes borrow from the
//! source. Edits locate methods through that tree and splice text into the original,
//! so everything they do not touch stays byte-identical.

pub mod ast;
mod parser;

pub use ast::{BlockKind, Class, Field, Instruction, Method, Operand, Statement};
pub use parser::{Error as ParseError, parse};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid smali: {0}")]
    Parse(#[from] ParseError),
    #[error("file declares {found}, not {expected}")]
    Class { expected: String, found: String },
    #[error("method {method}: expected {expected} definitions, found {found}")]
    Count {
        method: String,
        expected: usize,
        found: usize,
    },
    #[error("replacement must be exactly one method named {0}")]
    Replacement(String),
}

fn find<'a>(class: &'a Class<'a>, name: &str, signature: &str) -> Result<&'a Method<'a>, Error> {
    if class.name != name {
        return Err(Error::Class {
            expected: name.into(),
            found: class.name.into(),
        });
    }
    let matches: Vec<_> = class
        .methods
        .iter()
        .filter(|m| m.signature() == signature)
        .collect();
    match matches.as_slice() {
        [method] => Ok(method),
        _ => Err(Error::Count {
            method: signature.into(),
            expected: 1,
            found: matches.len(),
        }),
    }
}

/// Replace method `signature` of `class` with `replacement`, one complete method.
pub fn replace_method(
    source: &str,
    class: &str,
    signature: &str,
    replacement: &str,
) -> Result<String, Error> {
    let parsed = parse(source)?;
    let span = find(&parsed, class, signature)?.span.clone();
    check_replacement(replacement, signature)?;
    Ok(splice(source, span, replacement))
}

/// Append `method`, whose signature must be `signature`, to `class`, which must not
/// already define it.
pub fn insert_method(
    source: &str,
    class: &str,
    signature: &str,
    method: &str,
) -> Result<String, Error> {
    let parsed = parse(source)?;
    let signature = check_replacement(method, signature)?;
    if parsed.name != class {
        return Err(Error::Class {
            expected: class.into(),
            found: parsed.name.into(),
        });
    }
    let found = parsed
        .methods
        .iter()
        .filter(|m| m.signature() == signature)
        .count();
    if found != 0 {
        return Err(Error::Count {
            method: signature,
            expected: 0,
            found,
        });
    }
    let mut output = source.to_owned();
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    output.push('\n');
    output.push_str(method);
    if !method.ends_with('\n') {
        output.push('\n');
    }
    Ok(output)
}

/// The replacement's signature, which must be `expected` when that is non-empty.
fn check_replacement(text: &str, expected: &str) -> Result<String, Error> {
    let wrapped = format!(".class public Lx;\n{text}\n");
    let parsed = parse(&wrapped)?;
    match parsed.methods.as_slice() {
        [method] if expected.is_empty() || method.signature() == expected => Ok(method.signature()),
        _ => Err(Error::Replacement(expected.into())),
    }
}

fn splice(source: &str, span: std::ops::Range<usize>, text: &str) -> String {
    let mut output = String::with_capacity(source.len() + text.len());
    output.push_str(&source[..span.start]);
    output.push_str(text);
    if !text.ends_with('\n') && source[..span.end].ends_with('\n') {
        output.push('\n');
    }
    output.push_str(&source[span.end..]);
    output
}
