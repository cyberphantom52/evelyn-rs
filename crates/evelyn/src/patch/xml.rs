//! XML edits on an xot tree. The edited document is written out
//! again in full, so its formatting may change; its content changes only where
//! the edits say.

use crate::recipe::{XmlMatch, XmlName, XmlOp, XmlPredicate, XmlStep};
use thiserror::Error;
use xot::{
    Node, Xot,
    output::xml::{Declaration, Parameters},
};

#[derive(Debug, Error)]
pub enum Error {
    #[error("XML document is not UTF-8")]
    Utf8,
    #[error("invalid XML")]
    Parse(#[source] xot::ParseError),
    #[error("XML edit failed")]
    InvalidXml,
    #[error("XML scalar edit cannot replace element children")]
    MixedContent,
    #[error("XML document element cannot be removed")]
    RemoveRoot,
    #[error("XML fragment must be one element")]
    Fragment,
    #[error("no resource type {0}")]
    ResourceType(String),
    #[error("the values file has no {kind} {name}")]
    MissingResource { kind: String, name: String },
    #[error("a {0} has an array's items, and a value one")]
    ResourceShape(String),
    #[error("a values file's document element is <resources>")]
    NotResources,
    #[error("XML could not be written")]
    Write(#[source] xot::Error),
}

/// The values file a resource of `kind` is in, as apkscope decodes them.
pub fn resource_file(kind: &str) -> Result<&'static str, Error> {
    Ok(match kind {
        "string" => "strings.xml",
        "bool" => "bools.xml",
        "integer" => "integers.xml",
        "color" => "colors.xml",
        "dimen" => "dimens.xml",
        "fraction" => "fractions.xml",
        "string-array" | "integer-array" | "array" => "arrays.xml",
        other => return Err(Error::ResourceType(other.to_owned())),
    })
}

/// Set the resource `<kind name=name>` of a values file to `value`: an array's
/// items, or one value as its text. One the file does not have is an error, or
/// with `create`, is added.
pub fn set_resource(
    input: &[u8],
    kind: &str,
    name: &str,
    value: &crate::recipe::ResourceValue,
    create: bool,
) -> Result<Vec<u8>, Error> {
    use crate::recipe::ResourceValue;
    let invalid = |_| Error::InvalidXml;
    let array = kind.ends_with("array");
    if array != matches!(value, ResourceValue::Items(_)) {
        return Err(Error::ResourceShape(kind.to_owned()));
    }
    let source = std::str::from_utf8(input).map_err(|_| Error::Utf8)?;
    let mut xot = Xot::new();
    let document = xot.parse(source).map_err(Error::Parse)?;
    let root = xot.document_element(document).map_err(invalid)?;
    let resources = xot.add_name("resources");
    if xot
        .element(root)
        .is_none_or(|element| element.name() != resources)
    {
        return Err(Error::NotResources);
    }
    let element_name = xot.add_name(kind);
    let name_attribute = xot.add_name("name");
    let found = xot.children(root).find(|node| {
        xot.element(*node)
            .is_some_and(|element| element.name() == element_name)
            && xot
                .attributes(*node)
                .get(name_attribute)
                .map(String::as_str)
                == Some(name)
    });
    let node = match found {
        Some(node) => node,
        None if create => {
            let node = xot.new_element(element_name);
            xot.attributes_mut(node)
                .insert(name_attribute, name.to_owned());
            xot.append(root, node).map_err(invalid)?;
            node
        }
        None => {
            return Err(Error::MissingResource {
                kind: kind.to_owned(),
                name: name.to_owned(),
            });
        }
    };
    let children: Vec<_> = xot.children(node).collect();
    for child in children {
        xot.remove(child).map_err(invalid)?;
    }
    match value {
        ResourceValue::One(text) => xot.append_text(node, text).map_err(invalid)?,
        ResourceValue::Items(items) => {
            let item = xot.add_name("item");
            for text in items {
                let child = xot.new_element(item);
                xot.append_text(child, text).map_err(invalid)?;
                xot.append(node, child).map_err(invalid)?;
            }
        }
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
        .map_err(Error::Write)?;
    Ok(output.into_bytes())
}

/// Later edits see earlier edits. A `PutChild` fragment must be exactly one element.
pub fn apply(input: &[u8], edits: &[XmlOp]) -> Result<Vec<u8>, Error> {
    let source = std::str::from_utf8(input).map_err(|_| Error::Utf8)?;
    let mut xot = Xot::new();
    let document = xot.parse(source).map_err(Error::Parse)?;
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
        .map_err(Error::Write)?;
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
        let uri = crate::recipe::escape_attribute(xot.namespace_str(namespace));
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

#[cfg(test)]
mod resource_tests {
    use super::*;
    use crate::recipe::ResourceValue;

    const VALUES: &str = r#"<?xml version="1.0" encoding="utf-8"?><resources><string name="a" translatable="false">old</string></resources>"#;

    fn set(kind: &str, name: &str, value: ResourceValue, create: bool) -> Result<String, Error> {
        set_resource(VALUES.as_bytes(), kind, name, &value, create)
            .map(|bytes| String::from_utf8(bytes).unwrap())
    }

    #[test]
    fn sets_a_value_and_keeps_its_attributes() {
        let out = set("string", "a", ResourceValue::One("new & <b>".into()), false).unwrap();
        assert!(
            out.contains(r#"<string name="a" translatable="false">new &amp; &lt;b&gt;</string>"#),
            "{out}"
        );
    }

    #[test]
    fn a_missing_one_needs_create() {
        assert!(matches!(
            set("string", "b", ResourceValue::One("x".into()), false),
            Err(Error::MissingResource { .. })
        ));
        let out = set(
            "integer-array",
            "b",
            ResourceValue::Items(vec!["1".into(), "2".into()]),
            true,
        )
        .unwrap();
        assert!(
            out.contains(r#"<integer-array name="b"><item>1</item><item>2</item></integer-array>"#),
            "{out}"
        );
    }

    #[test]
    fn an_array_takes_items() {
        assert!(matches!(
            set("string-array", "a", ResourceValue::One("x".into()), true),
            Err(Error::ResourceShape(_))
        ));
        assert!(matches!(
            resource_file("plurals"),
            Err(Error::ResourceType(_))
        ));
    }
}
