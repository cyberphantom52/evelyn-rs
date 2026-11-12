use crate::tree::{Element, Node};
use pest::{Parser, iterators::Pair};
use pest_derive::Parser;

#[derive(Parser)]
#[grammar = "src/smali.pest"]
struct Smali;

pub type Error = Box<pest::error::Error<Rule>>;

/// A `.smali` file as a tree.
pub fn parse(source: &str) -> Result<Node, Error> {
    parse_as(Rule::file, source)
}

/// What `rule` reads `source` as, all of it.
pub(crate) fn parse_as(rule: Rule, source: &str) -> Result<Node, Error> {
    Ok(node(Smali::parse(rule, source)?.next().expect("a rule")))
}

/// A rule's match as a node: its inner matches, and the text between them.
fn node(pair: Pair<'_, Rule>) -> Node {
    let (kind, text, start) = (pair.as_rule(), pair.as_str(), pair.as_span().start());
    let mut children = Vec::new();
    let mut at = 0;
    for inner in pair.into_inner().filter(|p| p.as_rule() != Rule::EOI) {
        let span = inner.as_span();
        if span.start() - start > at {
            children.push(Element::Text(text[at..span.start() - start].to_owned()));
        }
        at = span.end() - start;
        children.push(Element::Node(node(inner)));
    }
    if at < text.len() {
        children.push(Element::Text(text[at..].to_owned()));
    }
    Node { kind, children }
}
