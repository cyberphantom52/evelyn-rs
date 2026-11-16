//! Kits: operations defined out of the built-in ones, shipped with Evelyn in
//! `assets/kits/`. Every operation of every kit is available to every recipe,
//! called as a built-in one is, by a name no other operation has. A kit names
//! one `Outer` or `Outer(Inner)`; the second is called as `Outer(Inner(…))`.

use crate::recipe::{BUILT_IN_OPERATIONS, Error, Operation, ron_options};
use serde::{
    Deserialize, Deserializer,
    de::{self, DeserializeSeed, EnumAccess, MapAccess, VariantAccess, Visitor},
};
use std::{collections::BTreeMap, fmt, sync::OnceLock};

/// The kits shipped with Evelyn, by name and text.
const KITS: [(&str, &str); 1] = [(
    "oplus.ron",
    include_str!("../../../../assets/kits/oplus.ron"),
)];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Kit {
    /// The property files the ROM family's init loads after Android's own, in the
    /// order it loads them. A build without one leaves it out.
    #[serde(default)]
    property_files: Vec<String>,
    operations: BTreeMap<String, Definition>,
}

/// What a kit's operation does: with `{name}` bound to what each parameter is
/// given, or to its default, apply the operations, as a step does. What they
/// report skipping, the rest of the step still applies. An asset they name is
/// the calling recipe's.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    /// The parameters a call must give.
    pub parameters: Vec<String>,
    /// The parameters a call can give, by name, and what each is if it does not.
    #[serde(default)]
    pub defaults: BTreeMap<String, String>,
    /// Built-in operations: a kit's operation does not call another's.
    pub operations: Vec<Operation>,
}

/// The operations of every kit, once they are parsed.
pub(crate) static OPERATIONS: OnceLock<BTreeMap<String, Definition>> = OnceLock::new();

/// The property files the kits name, kit by kit, once they are parsed.
static PROPERTY_FILES: OnceLock<Vec<String>> = OnceLock::new();

/// The property files the kits' ROM families load after Android's, in order.
pub fn property_files() -> Result<&'static [String], Error> {
    operations()?;
    Ok(PROPERTY_FILES.get().map_or(&[], Vec::as_slice))
}

/// The operations of every kit, by name.
pub fn operations() -> Result<&'static BTreeMap<String, Definition>, Error> {
    if let Some(operations) = OPERATIONS.get() {
        return Ok(operations);
    }
    parse(Vec::new())
}

/// Parse Evelyn's kits and `more`, by name and text.
fn parse(more: Vec<(String, String)>) -> Result<&'static BTreeMap<String, Definition>, Error> {
    let mut operations = BTreeMap::new();
    let mut property_files = Vec::new();
    let kits = KITS
        .iter()
        .map(|(path, text)| (path.to_string(), text.to_string()))
        .chain(more);
    for (path, text) in kits {
        let kit: Kit = ron_options().from_str(&text).map_err(|source| Error::Ron {
            path: path.clone().into(),
            source: Box::new(source),
        })?;
        for file in kit.property_files {
            if !file.starts_with('/') || !file.ends_with(".prop") {
                return Err(Error::KitPropertyFile {
                    kit: path.to_string(),
                    file,
                });
            }
            property_files.push(file);
        }
        for (name, definition) in kit.operations {
            if BUILT_IN_OPERATIONS.contains(&name.as_str()) || operations.contains_key(&name) {
                return Err(Error::KitOperation {
                    kit: path.to_string(),
                    name,
                });
            }
            operations.insert(name, definition);
        }
    }
    PROPERTY_FILES.get_or_init(|| property_files);
    Ok(OPERATIONS.get_or_init(|| operations))
}

impl<'de> Deserialize<'de> for Operation {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_enum("Operation", &BUILT_IN_OPERATIONS, Calls)
    }
}

/// An operation by its name: a built-in one, parsed as it is declared, or a kit's.
struct Calls;

impl<'de> Visitor<'de> for Calls {
    type Value = Operation;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an operation")
    }

    fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> Result<Operation, A::Error> {
        let (Identifier(name), variant) = data.variant()?;
        if BUILT_IN_OPERATIONS.contains(&name.as_str()) {
            return Operation::deserialize(Replay { name, variant });
        }
        let operations = OPERATIONS.get().into_iter().flatten();
        let nests = format!("{name}(");
        if operations
            .clone()
            .any(|(known, _)| known.starts_with(&nests))
        {
            return variant.newtype_variant_seed(Nested(name));
        }
        let definition = operations
            .clone()
            .find_map(|(known, definition)| (*known == name).then_some(definition))
            .ok_or_else(|| de::Error::unknown_variant(&name, &BUILT_IN_OPERATIONS))?;
        let args = args(&name, definition, variant)?;
        Ok(Operation::Call { name, args })
    }
}

/// An operation's name, whatever it is.
struct Identifier(String);

impl<'de> Deserialize<'de> for Identifier {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Any;
        impl Visitor<'_> for Any {
            type Value = Identifier;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an operation's name")
            }
            fn visit_str<E: de::Error>(self, name: &str) -> Result<Identifier, E> {
                Ok(Identifier(name.to_owned()))
            }
        }
        deserializer.deserialize_identifier(Any)
    }
}

/// A built-in operation's name, given back with what follows it for the derived
/// parsing.
struct Replay<A> {
    name: String,
    variant: A,
}

impl<'de, A: VariantAccess<'de>> Deserializer<'de> for Replay<A> {
    type Error = A::Error;

    fn deserialize_any<T: Visitor<'de>>(self, visitor: T) -> Result<T::Value, A::Error> {
        visitor.visit_enum(self)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct enum identifier ignored_any
    }
}

impl<'de, A: VariantAccess<'de>> EnumAccess<'de> for Replay<A> {
    type Error = A::Error;
    type Variant = A;

    fn variant_seed<S: DeserializeSeed<'de>>(self, seed: S) -> Result<(S::Value, A), A::Error> {
        let name = seed.deserialize(de::value::StringDeserializer::<A::Error>::new(self.name))?;
        Ok((name, self.variant))
    }
}

/// What follows `Outer(`: the kit's `Outer(Inner)`, as `Inner(…)`.
struct Nested(String);

impl<'de> DeserializeSeed<'de> for Nested {
    type Value = Operation;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Operation, D::Error> {
        deserializer.deserialize_enum("Operation", &[], self)
    }
}

impl<'de> Visitor<'de> for Nested {
    type Value = Operation;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "one of the kit's {}(…)", self.0)
    }

    fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> Result<Operation, A::Error> {
        let (Identifier(inner), variant) = data.variant()?;
        let name = format!("{}({inner})", self.0);
        let definition = OPERATIONS
            .get()
            .and_then(|operations| operations.get(&name))
            .ok_or_else(|| de::Error::custom(format!("no kit defines {name}")))?;
        let args = args(&name, definition, variant)?;
        Ok(Operation::Call { name, args })
    }
}

/// What a call gives a kit operation's parameters: the one it must give, bare,
/// or any of them by name.
fn args<'de, A: VariantAccess<'de>>(
    name: &str,
    definition: &Definition,
    variant: A,
) -> Result<BTreeMap<String, String>, A::Error> {
    let parameters = &definition.parameters;
    let args = match variant.newtype_variant_seed(Args)? {
        Given::Bare(value) => match &parameters[..] {
            [parameter] => BTreeMap::from([(parameter.clone(), value)]),
            _ => {
                return Err(de::Error::custom(format!(
                    "{name} takes its parameters by name"
                )));
            }
        },
        Given::Named(args) => args,
    };
    if let Some(missing) = parameters.iter().find(|p| !args.contains_key(*p)) {
        return Err(de::Error::custom(format!("{name} needs {missing}")));
    }
    if let Some(extra) = args
        .keys()
        .find(|arg| !parameters.contains(arg) && !definition.defaults.contains_key(*arg))
    {
        return Err(de::Error::custom(format!(
            "{name} has no parameter {extra}"
        )));
    }
    Ok(args)
}

/// What a call gives: one string, or strings by name.
enum Given {
    Bare(String),
    Named(BTreeMap<String, String>),
}

struct Args;

impl<'de> DeserializeSeed<'de> for Args {
    type Value = Given;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Given, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Args {
    type Value = Given;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a string, or parameters by name")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Given, E> {
        Ok(Given::Bare(value.to_owned()))
    }

    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Given, M::Error> {
        let mut args = BTreeMap::new();
        while let Some((name, value)) = map.next_entry::<String, String>()? {
            args.insert(name, value);
        }
        Ok(Given::Named(args))
    }
}
