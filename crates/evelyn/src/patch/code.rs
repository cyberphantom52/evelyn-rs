//! The edits of a package's code, each a function over a class as apkscope
//! models it. A method is named as recipes write it, `name(params)return`, and
//! a method body or an operand is smali, read by apkscope's own parser.

use crate::recipe::Returns;
use apkscope_core::smali::model::{Class, Item, Method};
use apkscope_core::smali::text;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid smali")]
    Smali(#[from] apkscope_core::Error),
    #[error("method {method}: expected {expected} definitions, found {found}")]
    Count {
        method: String,
        expected: usize,
        found: usize,
    },
    #[error("replacement must be exactly one method named {0}")]
    Replacement(String),
    #[error("{0}: cannot stub a {1:?} method that way")]
    Stub(String, Returns),
}

/// Where in `class` the methods `signature` names are: as many as `expected`.
fn methods(class: &Class, signature: &str, expected: usize) -> Result<Vec<usize>, Error> {
    let (name, proto) = text::parse_signature(signature)?;
    let found: Vec<usize> = (0..class.methods.len())
        .filter(|at| class.methods[*at].name == name && class.methods[*at].proto == proto)
        .collect();
    match found.len() == expected {
        true => Ok(found),
        false => Err(Error::Count {
            method: signature.into(),
            expected,
            found: found.len(),
        }),
    }
}

/// The one method `body` is, whose signature must be `signature`.
fn method(body: &str, signature: &str) -> Result<Method, Error> {
    let method = text::parse_method(body)?;
    match text::parse_signature(signature)? == (method.name.clone(), method.proto.clone()) {
        true => Ok(method),
        false => Err(Error::Replacement(signature.into())),
    }
}

/// Replace method `signature` of `class` with `body`, one complete method.
pub(crate) fn replace_method(class: &mut Class, signature: &str, body: &str) -> Result<(), Error> {
    let at = methods(class, signature, 1)?[0];
    class.methods[at] = method(body, signature)?;
    Ok(())
}

/// Add `body`, whose signature must be `signature`, to `class`, which must not
/// already define it.
pub(crate) fn insert_method(class: &mut Class, signature: &str, body: &str) -> Result<(), Error> {
    methods(class, signature, 0)?;
    class.methods.push(method(body, signature)?);
    Ok(())
}

/// Replace the body of method `signature` so it only returns what `returns` says.
pub(crate) fn stub_method(
    class: &mut Class,
    signature: &str,
    returns: Returns,
) -> Result<(), Error> {
    let at = methods(class, signature, 1)?[0];
    let method = &mut class.methods[at];
    let body = match (returns, method.proto.ret.first()) {
        (Returns::Void, Some(b'V')) => ".locals 0\nreturn-void",
        (Returns::Zero, Some(b'J' | b'D')) => ".locals 2\nconst-wide/16 v0, 0x0\nreturn-wide v0",
        (Returns::Zero, Some(b'L' | b'[')) => ".locals 1\nconst/4 v0, 0x0\nreturn-object v0",
        (Returns::Zero, Some(b'Z' | b'B' | b'S' | b'C' | b'I' | b'F')) => {
            ".locals 1\nconst/4 v0, 0x0\nreturn v0"
        }
        (Returns::One, Some(b'Z' | b'B' | b'S' | b'C' | b'I')) => {
            ".locals 1\nconst/4 v0, 0x1\nreturn v0"
        }
        _ => return Err(Error::Stub(signature.into(), returns)),
    };
    // The stub's registers are its locals: the method's own parameters follow.
    let stub = text::parse_method(&format!(".method static stub()V\n{body}\n.end method\n"))?;
    let mut code = stub.code.expect("the stub has a body");
    code.registers += method.ins();
    method.code = Some(code);
    Ok(())
}

/// Replace the operand `from` with `to`, in every instruction of `class`, or of
/// its method `signature`: a method, a field, a type, a string in its quotes, a
/// number. How many it replaced.
pub(crate) fn replace_operand(
    class: &mut Class,
    from: &str,
    to: &str,
    signature: Option<&str>,
) -> Result<usize, Error> {
    let (from, to) = (text::parse_operand(from)?, text::parse_operand(to)?);
    let only = signature.map(text::parse_signature).transpose()?;
    let mut replaced = 0;
    for method in &mut class.methods {
        if only
            .as_ref()
            .is_some_and(|(name, proto)| (&method.name, &method.proto) != (name, proto))
        {
            continue;
        }
        for item in method.code.iter_mut().flat_map(|code| &mut code.items) {
            if let Item::Ins(instruction) = item {
                replaced += usize::from(instruction.replace(&from, &to));
            }
        }
    }
    Ok(replaced)
}
