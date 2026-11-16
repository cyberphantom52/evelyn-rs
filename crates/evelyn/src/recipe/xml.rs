//! Literal XML selectors; paths include the document element.

use serde::Deserialize;
use thiserror::Error;

/// A selector or path a recipe writes that is not one.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SelectorError {
    #[error("invalid XML selector {0:?}")]
    Selector(String),
    #[error("invalid XML path {0:?}")]
    Path(String),
}

/// An element or attribute name, written as `local` or, with a namespace,
/// `{uri}local`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XmlName {
    pub namespace: Option<String>,
    pub local: String,
}

/// An element selector, written as a string: `name`, `name[@attr]`,
/// `name[@attr='value']`, `name[text()='value']` or `name[child='value']`, where an
/// element name may carry a namespace as `{uri}name` and `*` is any element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XmlMatch {
    pub name: XmlName,
    pub predicate: Option<XmlPredicate>,
}

impl TryFrom<String> for XmlMatch {
    type Error = SelectorError;

    fn try_from(text: String) -> Result<Self, SelectorError> {
        let local = |name: &str| XmlName::from(name.to_owned());
        let Some((name, rest)) = text.split_once('[') else {
            return Ok(Self {
                name: local(&text),
                predicate: None,
            });
        };
        let invalid = || SelectorError::Selector(text.clone());
        let inner = rest.strip_suffix(']').ok_or_else(invalid)?;
        let Some((key, value)) = inner.split_once('=') else {
            let attribute = inner.strip_prefix('@').ok_or_else(invalid)?;
            return Ok(Self {
                name: local(name),
                predicate: Some(XmlPredicate::HasAttribute {
                    name: local(attribute),
                }),
            });
        };
        let value = value
            .strip_prefix('\'')
            .and_then(|v| v.strip_suffix('\''))
            .or_else(|| value.strip_prefix('"').and_then(|v| v.strip_suffix('"')))
            .ok_or_else(invalid)?
            .to_owned();
        let predicate = if key == "text()" {
            XmlPredicate::Text { value }
        } else if let Some(attribute) = key.strip_prefix('@') {
            XmlPredicate::Attribute {
                name: local(attribute),
                value,
            }
        } else {
            XmlPredicate::ChildText {
                name: local(key),
                value,
            }
        };
        Ok(Self {
            name: local(name),
            predicate: Some(predicate),
        })
    }
}

impl std::fmt::Display for XmlName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.namespace {
            Some(namespace) => write!(f, "{{{namespace}}}{}", self.local),
            None => f.write_str(&self.local),
        }
    }
}

impl From<String> for XmlName {
    fn from(name: String) -> Self {
        match name.strip_prefix('{').and_then(|n| n.split_once('}')) {
            Some((namespace, local)) => Self {
                namespace: Some(namespace.to_owned()),
                local: local.to_owned(),
            },
            None => Self {
                namespace: None,
                local: name,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XmlPredicate {
    HasAttribute { name: XmlName },
    Attribute { name: XmlName, value: String },
    ChildText { name: XmlName, value: String },
    Text { value: String },
}

/// Element selectors from the document element down, written as one string:
/// `resources/string[@name='app_name']`. A step after `//` matches at any depth
/// below, not just among children: `//feature[@name='x']`. A `/` inside a
/// predicate or a `{namespace}` is part of it.
#[derive(Clone, Debug)]
pub struct XmlPath {
    pub selectors: Vec<XmlStep>,
}

/// One step of an `XmlPath`.
#[derive(Clone, Debug)]
pub struct XmlStep {
    /// Below at any depth, rather than among the children.
    pub descendant: bool,
    pub matches: XmlMatch,
}

impl TryFrom<String> for XmlPath {
    type Error = SelectorError;

    fn try_from(text: String) -> Result<Self, SelectorError> {
        let mut parts = Vec::new();
        let (mut start, mut quote, mut depth) = (0, None, 0);
        for (index, c) in text.char_indices() {
            match (c, quote) {
                ('\'' | '"', None) if depth > 0 => quote = Some(c),
                (c, Some(open)) if c == open => quote = None,
                ('[' | '{', None) => depth += 1,
                (']' | '}', None) => depth -= 1,
                ('/', None) if depth == 0 => {
                    parts.push(&text[start..index]);
                    start = index + 1;
                }
                _ => {}
            }
        }
        parts.push(&text[start..]);
        // An empty part is a `//`: the step after it matches at any depth.
        if parts.last().is_some_and(|part| part.is_empty()) {
            return Err(SelectorError::Path(text));
        }
        let mut selectors = Vec::new();
        let mut descendant = false;
        for part in parts {
            if part.is_empty() {
                descendant = true;
                continue;
            }
            selectors.push(XmlStep {
                descendant,
                matches: XmlMatch::try_from(part.to_owned())?,
            });
            descendant = false;
        }
        Ok(Self { selectors })
    }
}

/// An XML edit. Its paths, selectors, names and values are strings, in which
/// `{name}` is a bound value; they are parsed when the edit is applied.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum XmlEdit {
    /// Replace the text of every match.
    SetText { path: String, value: String },
    /// Set the attribute `name` of every match that has it to `value`; with
    /// `create`, add it to those that do not.
    SetAttribute {
        path: String,
        name: String,
        value: String,
        #[serde(default)]
        create: bool,
    },
    /// Remove every match.
    Remove { path: String },
    /// Put `element`, one element, in place of the children `key` matches under
    /// every match of `parent`; with `create`, append it where none match.
    PutChild {
        parent: String,
        key: String,
        element: String,
        #[serde(default)]
        create: bool,
    },
}

impl XmlEdit {
    /// The edit with each string made into what `f` makes of it.
    pub fn map<E>(&self, mut f: impl FnMut(&str) -> Result<String, E>) -> Result<Self, E> {
        Ok(match self {
            Self::SetText { path, value } => Self::SetText {
                path: f(path)?,
                value: f(value)?,
            },
            Self::SetAttribute {
                path,
                name,
                value,
                create,
            } => Self::SetAttribute {
                path: f(path)?,
                name: f(name)?,
                value: f(value)?,
                create: *create,
            },
            Self::Remove { path } => Self::Remove { path: f(path)? },
            Self::PutChild {
                parent,
                key,
                element,
                create,
            } => Self::PutChild {
                parent: f(parent)?,
                key: f(key)?,
                element: f(element)?,
                create: *create,
            },
        })
    }
}

/// An XML edit with its selectors parsed, as it is applied.
#[derive(Clone, Debug)]
pub enum XmlOp {
    SetText {
        path: XmlPath,
        value: String,
    },
    SetAttribute {
        path: XmlPath,
        name: XmlName,
        value: String,
        create: bool,
    },
    Remove {
        path: XmlPath,
    },
    PutChild {
        parent: XmlPath,
        key: XmlMatch,
        element: String,
        create: bool,
    },
}

impl XmlEdit {
    /// The edit with its selectors parsed.
    pub fn parse(&self) -> Result<XmlOp, SelectorError> {
        let path = |path: &String| XmlPath::try_from(path.clone());
        Ok(match self {
            Self::SetText { path: at, value } => XmlOp::SetText {
                path: path(at)?,
                value: value.clone(),
            },
            Self::SetAttribute {
                path: at,
                name,
                value,
                create,
            } => XmlOp::SetAttribute {
                path: path(at)?,
                name: XmlName::from(name.clone()),
                value: value.clone(),
                create: *create,
            },
            Self::Remove { path: at } => XmlOp::Remove { path: path(at)? },
            Self::PutChild {
                parent,
                key,
                element,
                create,
            } => XmlOp::PutChild {
                parent: path(parent)?,
                key: XmlMatch::try_from(key.clone())?,
                element: element.clone(),
                create: *create,
            },
        })
    }
}

/// Text as an XML attribute value.
pub fn escape_attribute(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
}
