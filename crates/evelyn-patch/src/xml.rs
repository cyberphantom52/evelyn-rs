//! XML edits on an xot tree. The edited document is written out
//! again in full, so its formatting may change; its content changes only where
//! the edits say.

use recipe::{XmlMatch, XmlName, XmlOp, XmlPredicate, XmlStep};
use thiserror::Error;
use xot::{
    Node, Xot,
    output::xml::{Declaration, Parameters},
};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("XML document is not UTF-8")]
    Utf8,
    #[error("invalid XML: {0}")]
    Parse(String),
    #[error("XML edit failed")]
    InvalidXml,
    #[error("XML scalar edit cannot replace element children")]
    MixedContent,
    #[error("XML document element cannot be removed")]
    RemoveRoot,
    #[error("XML fragment must be one element")]
    Fragment,
    #[error("XML could not be written: {0}")]
    Write(String),
}

/// Later edits see earlier edits. A `PutChild` fragment must be exactly one element.
pub fn apply(input: &[u8], edits: &[XmlOp]) -> Result<Vec<u8>, Error> {
    let source = std::str::from_utf8(input).map_err(|_| Error::Utf8)?;
    let mut xot = Xot::new();
    let document = xot
        .parse(source)
        .map_err(|error| Error::Parse(error.to_string()))?;
    let mut changed = false;
    for edit in edits {
        changed |= apply_one(&mut xot, document, edit)?;
    }
    // A document no edit changed is not written out again.
    if !changed {
        return Ok(input.to_vec());
    }
    let parameters = Parameters {
        declaration: Some(Declaration {
            encoding: Some("utf-8".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let output = xot
        .serialize_xml_string(parameters, document)
        .map_err(|error| Error::Write(error.to_string()))?;
    Ok(output.into_bytes())
}

/// Apply one edit; whether it changed the document.
fn apply_one(xot: &mut Xot, document: Node, edit: &XmlOp) -> Result<bool, Error> {
    let invalid = |_| Error::InvalidXml;
    match edit {
        XmlOp::SetText { path, value } => {
            let nodes = select(xot, document, &path.selectors);
            let changed = !nodes.is_empty();
            for node in nodes {
                if xot.children(node).any(|child| xot.is_element(child)) {
                    return Err(Error::MixedContent);
                }
                // Comments and processing instructions stay; the text is replaced.
                let texts: Vec<_> = xot.children(node).filter(|n| xot.is_text(*n)).collect();
                for text in texts {
                    xot.remove(text).map_err(invalid)?;
                }
                xot.append_text(node, value).map_err(invalid)?;
            }
            Ok(changed)
        }
        XmlOp::SetAttribute {
            path,
            name,
            value,
            create,
        } => {
            let nodes = select(xot, document, &path.selectors);
            let namespace = match &name.namespace {
                Some(uri) => xot.add_namespace(uri),
                None => xot.no_namespace(),
            };
            let id = xot.add_name_ns(&name.local, namespace);
            let mut changed = false;
            for node in nodes {
                let mut attributes = xot.attributes_mut(node);
                let there = attributes.get(id).is_some();
                if (there || *create) && attributes.get(id) != Some(value) {
                    attributes.insert(id, value.clone());
                    changed = true;
                }
            }
            Ok(changed)
        }
        XmlOp::Remove { path } => {
            let nodes = select(xot, document, &path.selectors);
            let root = xot.document_element(document).map_err(invalid)?;
            for node in &nodes {
                if *node == root {
                    return Err(Error::RemoveRoot);
                }
                // A match inside one already removed went with it.
                if !xot.is_removed(*node) {
                    xot.remove(*node).map_err(invalid)?;
                }
            }
            Ok(!nodes.is_empty())
        }
        XmlOp::PutChild {
            parent,
            key,
            element,
            create,
        } => {
            let parents = select(xot, document, &parent.selectors);
            let mut changed = false;
            for parent in &parents {
                let children: Vec<_> = xot
                    .children(*parent)
                    .filter(|node| matches(xot, *node, key))
                    .collect();
                match children.split_first() {
                    Some((first, rest)) => {
                        let fragment = fragment(xot, *parent, element)?;
                        xot.replace(*first, fragment).map_err(invalid)?;
                        for child in rest {
                            xot.remove(*child).map_err(invalid)?;
                        }
                    }
                    None if *create => {
                        let fragment = fragment(xot, *parent, element)?;
                        xot.append(*parent, fragment).map_err(invalid)?;
                    }
                    None => continue,
                }
                changed = true;
            }
            Ok(changed)
        }
    }
}

/// Parse `element` as it would read inside `parent`: with the namespace prefixes
/// (and default namespace) in scope there. The element comes out detached.
fn fragment(xot: &mut Xot, parent: Node, element: &str) -> Result<Node, Error> {
    let mut declarations = String::new();
    let xml = xot.xml_prefix();
    for (prefix, namespace) in xot.namespaces_in_scope(parent) {
        if prefix == xml {
            continue;
        }
        let uri = recipe::escape_attribute(xot.namespace_str(namespace));
        match xot.prefix_str(prefix) {
            "" => declarations.push_str(&format!(" xmlns=\"{uri}\"")),
            prefix => declarations.push_str(&format!(" xmlns:{prefix}=\"{uri}\"")),
        }
    }
    let wrapped = xot
        .parse(&format!("<fragment{declarations}>{element}</fragment>"))
        .map_err(|_| Error::Fragment)?;
    let wrapper = xot.document_element(wrapped).map_err(|_| Error::Fragment)?;
    let children: Vec<_> = xot.children(wrapper).collect();
    let [node] = children[..] else {
        return Err(Error::Fragment);
    };
    if !xot.is_element(node) {
        return Err(Error::Fragment);
    }
    xot.detach(node).map_err(|_| Error::Fragment)?;
    Ok(node)
}

/// Whether an xot name is `name`: its local name and namespace, if any.
fn same(xot: &Xot, id: xot::NameId, name: &XmlName) -> bool {
    let (local, uri) = xot.name_ns_str(id);
    local == name.local && Some(uri).filter(|uri| !uri.is_empty()) == name.namespace.as_deref()
}

/// An element named `name`, or with `*`, any element.
fn named(xot: &Xot, node: Node, name: &XmlName) -> bool {
    xot.element(node).is_some_and(|element| {
        (name.local == "*" && name.namespace.is_none()) || same(xot, element.name(), name)
    })
}

/// Text-only element whose text is `value`.
fn scalar(xot: &Xot, node: Node, value: &str) -> bool {
    if xot.children(node).any(|child| xot.is_element(child)) {
        return false;
    }
    let text: String = xot
        .children(node)
        .filter_map(|child| xot.text_str(child))
        .collect();
    text == value
}

fn matches(xot: &Xot, node: Node, selector: &XmlMatch) -> bool {
    if !named(xot, node, &selector.name) {
        return false;
    }
    match &selector.predicate {
        None => true,
        Some(XmlPredicate::HasAttribute { name }) => {
            xot.attributes(node).keys().any(|id| same(xot, id, name))
        }
        Some(XmlPredicate::Attribute { name, value }) => xot
            .attributes(node)
            .iter()
            .any(|(id, found)| same(xot, id, name) && found == value),
        Some(XmlPredicate::Text { value }) => scalar(xot, node, value),
        Some(XmlPredicate::ChildText { name, value }) => {
            let mut found = xot.children(node).filter(|child| named(xot, *child, name));
            match (found.next(), found.next()) {
                (Some(child), None) => scalar(xot, child, value),
                _ => false,
            }
        }
    }
}

fn select(xot: &Xot, document: Node, path: &[XmlStep]) -> Vec<Node> {
    let mut nodes = vec![document];
    for step in path {
        let mut next: Vec<Node> = Vec::new();
        for parent in nodes {
            // `descendants` starts with the node itself; a step selects below it.
            let below: Vec<Node> = match step.descendant {
                true => xot.descendants(parent).skip(1).collect(),
                false => xot.children(parent).collect(),
            };
            for node in below {
                if matches(xot, node, &step.matches) && !next.contains(&node) {
                    next.push(node);
                }
            }
        }
        nodes = next;
    }
    nodes
}
