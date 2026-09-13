//! Canonical release-record encoding and strict JSON decoding.

use std::fmt;

use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::error::{RailError, RailResult};
use crate::source::ContentDigest;

pub(crate) const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;

pub(crate) fn decode<T: serde::de::DeserializeOwned + serde::Serialize>(bytes: &[u8]) -> RailResult<T> {
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(RailError::message("release record exceeds 16 MiB"));
    }
    let value: UniqueValue = serde_json::from_slice(bytes)?;
    let decoded: T = serde_json::from_value(value.0.clone())?;
    if serde_json::to_value(&decoded)? != value.0 {
        return Err(RailError::message(
            "release record must contain the complete current contract without omitted or unknown fields",
        ));
    }
    Ok(decoded)
}

pub(crate) fn identity(domain: &str, value: Value) -> RailResult<String> {
    let bytes = canonical(value)?;
    let mut framed = Vec::with_capacity(domain.len() + bytes.len() + 1);
    framed.extend_from_slice(domain.as_bytes());
    framed.push(0);
    framed.extend_from_slice(&bytes);
    Ok(format!("sha256:{}", ContentDigest::sha256(&framed)))
}

pub(crate) fn canonical(value: Value) -> RailResult<Vec<u8>> {
    fn ordered(value: Value) -> RailResult<Value> {
        Ok(match value {
            Value::Object(object) => {
                let sorted = object.into_iter().collect::<std::collections::BTreeMap<_, _>>();
                Value::Object(
                    sorted
                        .into_iter()
                        .map(|(key, value)| Ok((key, ordered(value)?)))
                        .collect::<RailResult<_>>()?,
                )
            }
            Value::Array(array) => Value::Array(array.into_iter().map(ordered).collect::<RailResult<_>>()?),
            Value::Number(number) if !number.is_i64() && !number.is_u64() => {
                return Err(RailError::message(
                    "release records cannot contain floating-point numbers",
                ));
            }
            value => value,
        })
    }
    let bytes = serde_json::to_vec(&ordered(value)?)?;
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(RailError::message("release record exceeds 16 MiB"));
    }
    Ok(bytes)
}

struct UniqueValue(Value);
impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct JsonVisitor;
        impl<'de> Visitor<'de> for JsonVisitor {
            type Value = UniqueValue;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("JSON with unique fields and integer quantities")
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Null))
            }
            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Bool(value)))
            }
            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(UniqueValue(value.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(UniqueValue(value.into()))
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(UniqueValue(value.into()))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
                let mut array = Vec::new();
                while let Some(UniqueValue(value)) = sequence.next_element()? {
                    array.push(value);
                }
                Ok(UniqueValue(Value::Array(array)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut object: A) -> Result<Self::Value, A::Error> {
                let mut fields = serde_json::Map::new();
                while let Some(key) = object.next_key::<String>()? {
                    if fields.contains_key(&key) {
                        return Err(serde::de::Error::custom("duplicate release record field"));
                    }
                    let UniqueValue(value) = object.next_value()?;
                    fields.insert(key, value);
                }
                Ok(UniqueValue(Value::Object(fields)))
            }
        }
        deserializer.deserialize_any(JsonVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_orders_fields_but_preserves_array_order_and_domain() {
        let value = serde_json::json!({"z": "text", "a": [2, 1]});
        assert_eq!(canonical(value.clone()).unwrap(), br#"{"a":[2,1],"z":"text"}"#);
        assert_eq!(
            identity("cargo-rail-release-intent-v1", value).unwrap(),
            "sha256:879ea9d0c79ad91471fc241a09683f8f8e373b7b331c4fdb9ab3eb14acc79b06"
        );
    }

    #[test]
    fn decoder_rejects_nested_duplicate_fields_floats_and_trailing_values() {
        for bytes in [
            br#"{"intent":{"name":"a","name":"b"}}"#.as_slice(),
            br#"{"bytes":1.0}"#,
            br#"{"bytes":1} {}"#,
        ] {
            assert!(
                decode::<Value>(bytes).is_err(),
                "accepted {}",
                String::from_utf8_lossy(bytes)
            );
        }
        let value: Value = decode(br#"{"bytes":18446744073709551615}"#).unwrap();
        assert_eq!(value["bytes"].as_u64(), Some(u64::MAX));
    }
}
