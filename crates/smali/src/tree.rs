//! The smali tree. It is lossless: a node's children are the nodes inside it and
//! the text between them, in order, so printing a tree gives back what was parsed,
//! and an edit changes only the nodes it replaces.

use crate::Kind;
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// The grammar rule it matched.
    pub kind: Kind,
    pub children: Vec<Element>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Element {
    Node(Node),
    /// Spaces, punctuation, comments and line ends, or a leaf's own text.
    Text(String),
}

impl Node {
    /// The nodes directly inside it.
    pub fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.children.iter().filter_map(|child| match child {
            Element::Node(node) => Some(node),
            Element::Text(_) => None,
        })
    }

    /// The first node of `kind` directly inside it.
    pub fn child(&self, kind: Kind) -> Option<&Node> {
        self.nodes().find(|node| node.kind == kind)
    }

    /// Call `visit` with it and then with every node inside it, in order.
    pub fn visit(&mut self, visit: &mut impl FnMut(&mut Node)) {
        visit(self);
        for child in &mut self.children {
            if let Element::Node(node) = child {
                node.visit(visit);
            }
        }
    }
}

impl fmt::Display for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.children.iter().try_for_each(|child| match child {
            Element::Node(node) => node.fmt(f),
            Element::Text(text) => f.write_str(text),
        })
    }
}
