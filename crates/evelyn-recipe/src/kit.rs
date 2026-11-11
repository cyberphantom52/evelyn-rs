//! Kits: operations defined out of the built-in ones, shipped with Evelyn in
//! `assets/kits/`. Every operation of every kit is available to every recipe,
//! called as a built-in one is, by a name no other operation has.

use crate::{BUILT_IN, Error, Expr, Operation, ron_options};
use android::firmware::Name;
use serde::{
    Deserialize, Deserializer,
    de::{self, DeserializeSeed, EnumAccess, MapAccess, VariantAccess, Visitor},
};
use std::{collections::BTreeMap, fmt, sync::OnceLock};

/// The kits shipped with Evelyn, by name and text.
const KITS: [(&str, &str); 1] = [("oplus.ron", include_str!("../../../assets/kits/oplus.ron"))];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Kit {
    operations: BTreeMap<Name, Definition>,
}

/// What a kit's operation does, as a step does it: with `{name}` bound to what
/// each parameter is given, and to what `let:` binds, if `when:` holds, apply
/// the operations. A value that is not set, or a condition that does not hold,
/// skips them, and is reported; the rest of the step still applies.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub parameters: Vec<Name>,
    #[serde(rename = "let", default)]
    pub bindings: BTreeMap<Name, Expr>,
    #[serde(default)]
    pub when: Option<Expr>,
    /// Built-in operations: a kit's operation does not call another's.
    pub operations: Vec<Operation>,
}

/// The operations of every kit, once they are parsed.
pub(crate) static OPERATIONS: OnceLock<BTreeMap<Name, Definition>> = OnceLock::new();

/// The operations of every kit, by name.
pub fn operations() -> Result<&'static BTreeMap<Name, Definition>, Error> {
    if let Some(operations) = OPERATIONS.get() {
        return Ok(operations);
    }
    let mut operations = BTreeMap::new();
    for (path, text) in KITS {
        let kit: Kit = ron_options().from_str(text).map_err(|source| Error::Ron {
            path: path.into(),
            source: Box::new(source),
        })?;
        for (name, definition) in kit.operations {
            if BUILT_IN.contains(&name.as_ref()) || operations.contains_key(&name) {
                return Err(Error::Invalid(format!(
                    "{path} defines {name}, which is already an operation"
                )));
            }
            operations.insert(name, definition);
        }
    }
    Ok(OPERATIONS.get_or_init(|| operations))
}

impl<'de> Deserialize<'de> for Operation {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_enum("Operation", &BUILT_IN, Calls)
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
        if BUILT_IN.contains(&name.as_str()) {
            return Operation::deserialize(Replay { name, variant });
        }
        let unknown = || de::Error::unknown_variant(&name, &BUILT_IN);
        let call = Name::try_from(name.clone()).map_err(|_| unknown())?;
        let definition = OPERATIONS
            .get()
            .and_then(|operations| operations.get(&call))
            .ok_or_else(unknown)?;
        let args = match &definition.parameters[..] {
            [] => variant.unit_variant().map(|()| BTreeMap::new())?,
            [parameter] => BTreeMap::from([(parameter.clone(), variant.newtype_variant()?)]),
            parameters => {
                let args: BTreeMap<Name, String> = variant.struct_variant(&[], Args)?;
                if let Some(missing) = parameters.iter().find(|p| !args.contains_key(*p)) {
                    return Err(de::Error::custom(format!("{name} needs {missing}")));
                }
                if let Some(extra) = args.keys().find(|arg| !parameters.contains(arg)) {
                    return Err(de::Error::custom(format!(
                        "{name} has no parameter {extra}"
                    )));
                }
                args
            }
        };
        Ok(Operation::Call { name: call, args })
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

/// A kit operation's parameters, given by name.
struct Args;

impl<'de> Visitor<'de> for Args {
    type Value = BTreeMap<Name, String>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("parameters by name")
    }

    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
        let mut args = BTreeMap::new();
        while let Some((name, value)) = map.next_entry::<String, String>()? {
            args.insert(Name::try_from(name).map_err(de::Error::custom)?, value);
        }
        Ok(args)
    }
}
