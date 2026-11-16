//! Smali parsing and editing.
//!
//! [`parse`] turns a `.smali` file into a lossless tree of [`Node`]s, which prints
//! back as it was written. Edits replace nodes of that tree, so everything they do
//! not touch stays byte-identical.

mod parser;
mod tree;

pub use parser::{Error as ParseError, Rule as Kind, parse};
pub use tree::{Element, Node};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid smali")]
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

/// What a stubbed method returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stub {
    /// `return-void`, for `V` methods.
    Void,
    /// `0`/`false`/`null` for the method's return type.
    Zero,
    /// `1`/`true`, for `Z`, `B`, `S`, `C` and `I` methods.
    One,
}

/// `name(params)return` of a method or a method reference, the form recipes use
/// to select a method.
fn signature(method: &Node) -> String {
    let part = |kind| method.child(kind).map(Node::to_string).unwrap_or_default();
    part(Kind::member_name) + &part(Kind::proto)
}

/// Where in `file`, which must declare class `name`, the methods `signature`
/// names are: as many as `expected`.
fn methods(file: &Node, name: &str, signature: &str, expected: usize) -> Result<Vec<usize>, Error> {
    let class = file
        .child(Kind::class)
        .and_then(|class| class.child(Kind::class_type));
    let found = class.map(Node::to_string).unwrap_or_default();
    if found != name {
        return Err(Error::Class {
            expected: name.into(),
            found,
        });
    }
    let matches: Vec<usize> = (0..file.children.len())
        .filter(|at| match &file.children[*at] {
            Element::Node(node) => node.kind == Kind::method && self::signature(node) == signature,
            Element::Text(_) => false,
        })
        .collect();
    if matches.len() != expected {
        return Err(Error::Count {
            method: signature.into(),
            expected,
            found: matches.len(),
        });
    }
    Ok(matches)
}

/// The one method `text` is, whose signature must be `expected`.
fn method(text: &str, expected: &str) -> Result<Node, Error> {
    let file = parse(&format!("{}\n", text.trim_end_matches('\n')))?;
    let mut methods = file.nodes().filter(|node| node.kind == Kind::method);
    match (methods.next(), methods.next()) {
        (Some(method), None) if signature(method) == expected => Ok(method.clone()),
        _ => Err(Error::Replacement(expected.into())),
    }
}

/// Replace method `signature` of `class` with `replacement`, one complete method.
pub fn replace_method(
    source: &str,
    class: &str,
    signature: &str,
    replacement: &str,
) -> Result<String, Error> {
    let mut file = parse(source)?;
    let at = methods(&file, class, signature, 1)?[0];
    file.children[at] = Element::Node(method(replacement, signature)?);
    Ok(file.to_string())
}

/// Append `method`, whose signature must be `signature`, to `class`, which must not
/// already define it.
pub fn insert_method(
    source: &str,
    class: &str,
    signature: &str,
    method: &str,
) -> Result<String, Error> {
    let mut file = parse(source)?;
    methods(&file, class, signature, 0)?;
    let method = self::method(method, signature)?;
    let gap = match source.is_empty() || source.ends_with('\n') {
        true => "\n",
        false => "\n\n",
    };
    file.children.push(Element::Text(gap.into()));
    file.children.push(Element::Node(method));
    Ok(file.to_string())
}

/// Replace the body of method `signature` so it only returns `stub`.
pub fn stub_method(
    source: &str,
    class: &str,
    signature: &str,
    stub: Stub,
) -> Result<String, Error> {
    let mut file = parse(source)?;
    let at = methods(&file, class, signature, 1)?[0];
    let Element::Node(method) = &mut file.children[at] else {
        unreachable!("methods are nodes")
    };
    let proto = method.child(Kind::proto).expect("a method has a proto");
    let returns = proto
        .nodes()
        .last()
        .map(Node::to_string)
        .unwrap_or_default();
    let body = match (stub, returns.as_bytes().first()) {
        (Stub::Void, Some(b'V')) => "    .locals 0\n\n    return-void\n",
        (Stub::Zero, Some(b'J' | b'D')) => {
            "    .locals 2\n\n    const-wide/16 v0, 0x0\n\n    return-wide v0\n"
        }
        (Stub::Zero, Some(b'L' | b'[')) => {
            "    .locals 1\n\n    const/4 v0, 0x0\n\n    return-object v0\n"
        }
        (Stub::Zero, Some(b'Z' | b'B' | b'S' | b'C' | b'I' | b'F')) => {
            "    .locals 1\n\n    const/4 v0, 0x0\n\n    return v0\n"
        }
        (Stub::One, Some(b'Z' | b'B' | b'S' | b'C' | b'I')) => {
            "    .locals 1\n\n    const/4 v0, 0x1\n\n    return v0\n"
        }
        _ => {
            return Err(Error::Replacement(format!(
                "{signature}: cannot stub a {returns} method that way"
            )));
        }
    };
    // The statements are what follows the proto's line end, up to `.end method`.
    let header = method.children.iter().position(|child| match child {
        Element::Node(node) => node.kind == Kind::proto,
        Element::Text(_) => false,
    });
    let stub = parse(&format!(".method stub()V\n{body}.end method\n"))?;
    let Some(Element::Node(stub)) = stub.children.into_iter().next() else {
        unreachable!("the stub is a method")
    };
    let statements = stub.children.into_iter().skip(3);
    method
        .children
        .truncate(header.expect("a method has a proto") + 1);
    method.children.extend(statements);
    Ok(file.to_string())
}

/// Replace the operand written as `from` with `to`, in every instruction of the
/// file, or of its method `signature`: a method, a field, a type, a string in its
/// quotes, a number, a register. Says how many it replaced.
pub fn replace_operand(
    source: &str,
    from: &str,
    to: &str,
    signature: Option<&str>,
) -> Result<(String, usize), Error> {
    let mut file = parse(source)?;
    let to = parser::parse_as(Kind::operand_only, to)?;
    let mut replaced = 0;
    for child in &mut file.children {
        let Element::Node(method) = child else {
            continue;
        };
        if method.kind != Kind::method || signature.is_some_and(|s| self::signature(method) != s) {
            continue;
        }
        method.visit(&mut |node| {
            if node.kind != Kind::instruction {
                return;
            }
            for operand in &mut node.children {
                if let Element::Node(operand) = operand
                    && operand.kind != Kind::opcode
                    && operand.to_string() == from
                {
                    *operand = to.clone();
                    replaced += 1;
                }
            }
        });
    }
    Ok((file.to_string(), replaced))
}
