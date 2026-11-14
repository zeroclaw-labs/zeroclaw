//! Presence belongs to the patch, not to a resolved request parameter.

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};

/// `None` on the containing field inherits; `Clear` explicitly removes a value.
#[derive(Debug, Clone, PartialEq)]
pub enum Override<T> {
    Set(T),
    Clear,
}

impl<T> Override<T> {
    /// Consume a final, already merged patch. Callers supply defaults only for
    /// an absent patch, never after this conversion.
    pub fn into_option(self) -> Option<T> {
        match self {
            Self::Set(value) => Some(value),
            Self::Clear => None,
        }
    }

    pub fn as_ref(&self) -> Override<&T> {
        match self {
            Self::Set(value) => Override::Set(value),
            Self::Clear => Override::Clear,
        }
    }
}

impl<T: Serialize> Serialize for Override<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Clear => serializer.serialize_str("null"),
            Self::Set(value) => {
                // Only the reserved string needs escaping. All ordinary Set
                // values retain their normal TOML/JSON representation.
                let raw = serde_json::to_value(value).map_err(serde::ser::Error::custom)?;
                if raw.as_str() == Some("null") {
                    use serde::ser::SerializeMap;
                    let mut map = serializer.serialize_map(Some(1))?;
                    map.serialize_entry("value", value)?;
                    map.end()
                } else {
                    value.serialize(serializer)
                }
            }
        }
    }
}

impl<'de, T: DeserializeOwned> Deserialize<'de> for Override<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = serde_json::Value::deserialize(deserializer)?;
        if raw.as_str() == Some("null") {
            return Ok(Self::Clear);
        }
        let value = match raw {
            serde_json::Value::Object(mut object) => {
                if object.len() != 1 || object.get("value").and_then(|v| v.as_str()) != Some("null")
                {
                    return Err(serde::de::Error::custom(
                        "only { value = \"null\" } escapes the reserved null string",
                    ));
                }
                object
                    .remove("value")
                    .ok_or_else(|| serde::de::Error::custom("missing literal null escape value"))?
            }
            value => value,
        };
        serde_json::from_value(value)
            .map(Self::Set)
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "schema-export")]
impl<T: schemars::JsonSchema> schemars::JsonSchema for Override<T> {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        format!("Override_{}", T::schema_name()).into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut schema = schemars::json_schema!({
            "anyOf": [
                { "const": "null" },
                {
                    "type": "object",
                    "properties": { "value": { "const": "null" } },
                    "required": ["value"],
                    "additionalProperties": false
                }
            ]
        });
        if let Some(alternatives) = schema
            .get_mut("anyOf")
            .and_then(|value| value.as_array_mut())
        {
            alternatives.insert(0, generator.subschema_for::<T>().to_value());
        }
        schema
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU64;

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(bound(deserialize = "T: DeserializeOwned"))]
    struct Patch<T> {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<Override<T>>,
    }

    #[test]
    fn presence_and_explicit_values_survive_toml_roundtrip() {
        for input in ["", "value = false", "value = true", "value = \"null\""] {
            let parsed: Patch<bool> = toml::from_str(input).unwrap();
            let encoded = toml::to_string(&parsed).unwrap();
            let reloaded: Patch<bool> = toml::from_str(&encoded).unwrap();
            assert_eq!(parsed.value, reloaded.value);
        }
        assert!(toml::from_str::<Patch<bool>>("").unwrap().value.is_none());
        assert_eq!(
            toml::from_str::<Patch<bool>>("value = false")
                .unwrap()
                .value,
            Some(Override::Set(false))
        );
        assert_eq!(
            toml::from_str::<Patch<bool>>("value = \"null\"")
                .unwrap()
                .value,
            Some(Override::Clear)
        );
    }

    #[test]
    fn literal_null_is_distinct_and_escape_shape_is_strict() {
        let parsed: Patch<String> = toml::from_str("value = { value = \"null\" }").unwrap();
        assert_eq!(parsed.value, Some(Override::Set("null".into())));
        let reloaded: Patch<String> = toml::from_str(&toml::to_string(&parsed).unwrap()).unwrap();
        assert_eq!(parsed.value, reloaded.value);
        for input in [
            "value = { value = \"ordinary\" }",
            "value = { value = \"null\", extra = 1 }",
        ] {
            assert!(toml::from_str::<Patch<String>>(input).is_err());
        }
    }

    #[test]
    fn zero_is_not_an_infinite_timeout() {
        for input in ["value = 0", "value = -1", "value = \"\"", "value = true"] {
            assert!(
                toml::from_str::<Patch<NonZeroU64>>(input).is_err(),
                "{input}"
            );
        }
        assert_eq!(
            toml::from_str::<Patch<NonZeroU64>>("value = \"null\"")
                .unwrap()
                .value,
            Some(Override::Clear)
        );
    }
}
