use crate::{Error, Limits};
use serde::de::{DeserializeOwned, DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use std::fmt;
use std::io::{self, Write};

/// JSON is capped before parsing. The visitor rejects duplicate keys everywhere,
/// including maps that ordinary serde_json would silently overwrite. Its own
/// depth/node budgets precede recursive allocations; serde_json's recursion
/// limit remains enabled as a second, independent guard.
pub(crate) fn parse<T: DeserializeOwned>(bytes: &[u8], limits: &Limits) -> Result<T, Error> {
    limits.validate()?;
    if bytes.len() > limits.max_input_bytes {
        return Err(Error::Limit("input bytes".into()));
    }
    let mut nodes = 0;
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let value = Seed {
        limits,
        nodes: &mut nodes,
        depth: 0,
    }
    .deserialize(&mut decoder)
    .map_err(|e| Error::InvalidInput(e.to_string()))?;
    decoder
        .end()
        .map_err(|e| Error::InvalidInput(e.to_string()))?;
    serde_json::from_value(value).map_err(|e| Error::InvalidInput(e.to_string()))
}

struct Seed<'a> {
    limits: &'a Limits,
    nodes: &'a mut usize,
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = Value;
    fn deserialize<D: serde::Deserializer<'de>>(self, decoder: D) -> Result<Value, D::Error> {
        if self.depth > self.limits.max_json_depth || *self.nodes >= self.limits.max_json_nodes {
            return Err(D::Error::custom("JSON depth/node budget exhausted"));
        }
        *self.nodes += 1;
        decoder.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Seed<'_> {
    type Value = Value;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bounded JSON")
    }
    fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }
    fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }
    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }
    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Value, E> {
        Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| E::custom("non-finite number"))
    }
    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Value, E> {
        if v.len() > self.limits.max_string_bytes {
            return Err(E::custom("string budget exhausted"));
        }
        Ok(Value::String(v.into()))
    }
    fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Value, E> {
        if v.len() > self.limits.max_string_bytes {
            return Err(E::custom("string budget exhausted"));
        }
        Ok(Value::String(v))
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_none<E: serde::de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = seq.next_element_seed(Seed {
            limits: self.limits,
            nodes: self.nodes,
            depth: self.depth + 1,
        })? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut values = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if key.len() > self.limits.max_string_bytes {
                return Err(A::Error::custom("key budget exhausted"));
            }
            if values.contains_key(&key) {
                return Err(A::Error::custom(format!("duplicate JSON key: {key}")));
            }
            let value = map.next_value_seed(Seed {
                limits: self.limits,
                nodes: self.nodes,
                depth: self.depth + 1,
            })?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}

/// Bounds programmatic DTOs too, without allocating a serialized copy first.
pub(crate) fn check_size<T: serde::Serialize>(value: &T, limits: &Limits) -> Result<(), Error> {
    struct Counter {
        left: usize,
    }
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.left = self
                .left
                .checked_sub(bytes.len())
                .ok_or_else(|| io::Error::other("input bytes budget exhausted"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(
        Counter {
            left: limits.max_input_bytes,
        },
        value,
    )
    .map_err(|_| Error::Limit("programmatic input bytes".into()))
}
