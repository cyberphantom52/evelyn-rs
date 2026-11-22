//! The edits of a package's code, each a function over a class as apkscope
//! models it, with the source its ids are of. A method is named as recipes write
//! it, `name(params)return`, and a method body or an operand is smali, read by
//! apkscope's own parser: what it names that the file lacks is added to the
//! source.

use crate::recipe::Returns;
use apkscope_core::smali::model::{Class, Item, MethodId, Source};
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

/// The method `signature` names, and where `class` defines it: in as many places
/// as `expected`.
fn methods(
    src: &Source,
    class: &Class,
    signature: &str,
    expected: usize,
) -> Result<(Option<MethodId>, Vec<usize>), Error> {
    let id = text::find_method(src, class.name, signature)?;
    let found: Vec<usize> = (0..class.methods.len())
        .filter(|at| Some(class.methods[*at].id) == id)
        .collect();
    match found.len() == expected {
        true => Ok((id, found)),
        false => Err(Error::Count {
            method: signature.into(),
            expected,
            found: found.len(),
        }),
    }
}

/// Replace method `signature` of `class` with `body`, one complete method of that
/// signature.
pub(crate) fn replace_method(
    src: &mut Source,
    class: &mut Class,
    signature: &str,
    body: &str,
) -> Result<(), Error> {
    let (id, at) = methods(src, class, signature, 1)?;
    let method = text::parse_method(src, class.name, body)?;
    if Some(method.id) != id {
        return Err(Error::Replacement(signature.into()));
    }
    class.methods[at[0]] = method;
    Ok(())
}

/// Add `body`, whose signature must be `signature`, to `class`, which must not
/// already define it.
pub(crate) fn insert_method(
    src: &mut Source,
    class: &mut Class,
    signature: &str,
    body: &str,
) -> Result<(), Error> {
    methods(src, class, signature, 0)?;
    let method = text::parse_method(src, class.name, body)?;
    if text::find_method(src, class.name, signature)? != Some(method.id) {
        return Err(Error::Replacement(signature.into()));
    }
    class.methods.push(method);
    Ok(())
}

/// Replace the body of method `signature` so it only returns what `returns` says.
pub(crate) fn stub_method(
    src: &mut Source,
    class: &mut Class,
    signature: &str,
    returns: Returns,
) -> Result<(), Error> {
    let (id, at) = methods(src, class, signature, 1)?;
    let ret = src.proto(src.method(id.expect("found once")).proto).ret;
    let body = match (returns, src.descriptor(ret).first()) {
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
    // The stub is parsed as a method of this signature, so it names nothing the
    // source lacks; its registers are its locals and the parameters it is
    // written with, in place of which the method has its own.
    let text = format!(".method static {signature}\n{body}\n.end method\n");
    let stub = text::parse_method(src, class.name, &text)?;
    let method = &mut class.methods[at[0]];
    let mut code = stub.code.clone().expect("the stub has a body");
    code.registers = code.registers - stub.ins(src) + method.ins(src);
    method.code = Some(code);
    Ok(())
}

/// Replace the operand `from` with `to`, both smali, in every instruction of
/// `class`, or of its method `signature`: a method, a field, a type, a string in
/// its quotes, a number. How many it replaced: none where the source lacks what
/// `from` names, as no class of it has that.
pub(crate) fn replace_operand(
    src: &mut Source,
    class: &mut Class,
    from: &str,
    to: &str,
    signature: Option<&str>,
) -> Result<usize, Error> {
    let Some(from) = text::find_operand(src, from)? else {
        return Ok(0);
    };
    let only = match signature {
        Some(signature) => match text::find_method(src, class.name, signature)? {
            Some(id) => Some(id),
            None => return Ok(0),
        },
        None => None,
    };
    let to = text::parse_operand(src, to)?;
    let mut replaced = 0;
    for method in &mut class.methods {
        if only.is_some_and(|only| method.id != only) {
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
