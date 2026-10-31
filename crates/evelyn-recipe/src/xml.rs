//! Literal XML selectors; paths include the document element.

use crate::Error;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XmlName {
    #[serde(default)]
    pub namespace: Option<String>,
    pub local: String,
}

/// An element selector. Recipes may write it in full or as a string:
/// `name`, `name[@attr='value']`, `name[text()='value']` or `name[child='value']`,
/// where an element name may carry a namespace as `{uri}name`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "MatchForm")]
pub struct XmlMatch {
    pub name: XmlName,
    #[serde(default)]
    pub predicate: Option<XmlPredicate>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum MatchForm {
    Short(String),
    Full {
        name: XmlName,
        #[serde(default)]
        predicate: Option<XmlPredicate>,
    },
}

impl TryFrom<MatchForm> for XmlMatch {
    type Error = String;

    fn try_from(form: MatchForm) -> Result<Self, String> {
        let text = match form {
            MatchForm::Full { name, predicate } => return Ok(Self { name, predicate }),
            MatchForm::Short(text) => text,
        };
        let local = |name: &str| match name.strip_prefix('{').and_then(|n| n.split_once('}')) {
            Some((namespace, local)) => XmlName {
                namespace: Some(namespace.to_owned()),
                local: local.to_owned(),
            },
            None => XmlName {
                namespace: None,
                local: name.to_owned(),
            },
        };
        let Some((name, rest)) = text.split_once('[') else {
            return Ok(Self {
                name: local(&text),
                predicate: None,
            });
        };
        let invalid = || format!("invalid XML selector {text:?}");
        let (key, value) = rest
            .strip_suffix(']')
            .and_then(|inner| inner.split_once('='))
            .ok_or_else(invalid)?;
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum XmlPredicate {
    Attribute { name: XmlName, value: String },
    ChildText { name: XmlName, value: String },
    Text { value: String },
}

/// Element selectors from the document element down, written as one string:
/// `resources/string[@name='app_name']`. A step after `//` matches at any depth
/// below, not just among children: `//feature[@name='x']`. A `/` inside a
/// predicate or a `{namespace}` is part of it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct XmlPath {
    text: String,
    pub selectors: Vec<XmlStep>,
}

/// One step of an `XmlPath`.
#[derive(Clone, Debug)]
pub struct XmlStep {
    /// Below at any depth, rather than among the children.
    pub descendant: bool,
    pub matches: XmlMatch,
}

impl XmlPath {
    /// Every element `matches` selects, at any depth.
    pub(crate) fn anywhere(matches: XmlMatch, text: String) -> Self {
        Self {
            text,
            selectors: vec![XmlStep {
                descendant: true,
                matches,
            }],
        }
    }
}

impl TryFrom<String> for XmlPath {
    type Error = String;

    fn try_from(text: String) -> Result<Self, String> {
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
            return Err(format!("invalid XML path {text:?}"));
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
                matches: XmlMatch::try_from(MatchForm::Short(part.to_owned()))?,
            });
            descendant = false;
        }
        Ok(Self { text, selectors })
    }
}

impl From<XmlPath> for String {
    fn from(path: XmlPath) -> Self {
        path.text
    }
}

fn one() -> usize {
    1
}

fn is_one(expect: &usize) -> bool {
    *expect == 1
}

fn some_one() -> Option<usize> {
    Some(1)
}

fn is_some_one(expect: &Option<usize>) -> bool {
    *expect == Some(1)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum XmlEdit {
    /// Replace the text of every match; `expect` (default 1) pins how many.
    SetText {
        path: XmlPath,
        #[serde(default = "one", skip_serializing_if = "is_one")]
        expect: usize,
        value: String,
    },
    /// Remove every match; `expect` (default 1) pins how many, and `None` lets
    /// there be any number, none included.
    Remove {
        path: XmlPath,
        #[serde(default = "some_one", skip_serializing_if = "is_some_one")]
        expect: Option<usize>,
    },
    /// Add `element` under `parent`, replacing the child `key` matches if there is
    /// one. `expect` pins whether there must be (1) or must not be (0).
    PutChild {
        parent: XmlPath,
        key: XmlMatch,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect: Option<usize>,
        element: String,
    },
}

/// Shape rules the editor relies on; names, values and fragments are checked when applied.
pub(crate) fn validate(edits: &[XmlEdit]) -> Result<(), Error> {
    for edit in edits {
        if let XmlEdit::PutChild {
            expect: Some(2..), ..
        } = edit
        {
            return Err(Error::Invalid(
                "XML PutChild expects 0 or 1 existing child".into(),
            ));
        }
    }
    Ok(())
}

/// Text as an XML attribute value.
pub fn escape_attribute(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
}
