//! Typed slash-command option model (contract tier): the option shapes that
//! flow from a skill's `[[skill.slash_options]]` manifest declaration into the
//! Discord application-command registration body. Pure data plus the trivial
//! serialization to Discord's option JSON — no IO, no runtime types. Imported
//! by `types` (the command spec carries a `Vec<OptionSpec>`) and by `slash`
//! (which maps skill declarations into these and builds the registration body);
//! imports no sibling impl module, so the contract layer stays acyclic.

use serde_json::{Map, Value, json};

/// A Discord application-command option type this channel supports. The wire
/// integer is Discord's `ApplicationCommandOptionType`. (Sub-commands/groups —
/// types 1/2 — are intentionally out of scope here; flat options only.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptKind {
    String,
    Integer,
    Number,
    Boolean,
    User,
    Channel,
    Role,
    Mentionable,
}

impl OptKind {
    /// Parse a skill-manifest `type` string. Unknown values return `None` (the
    /// channel drops the option with a WARN rather than registering a bad type).
    pub fn from_manifest(kind: &str) -> Option<Self> {
        match kind.trim().to_ascii_lowercase().as_str() {
            "string" | "str" | "text" => Some(Self::String),
            "integer" | "int" => Some(Self::Integer),
            "number" | "float" | "double" => Some(Self::Number),
            "boolean" | "bool" => Some(Self::Boolean),
            "user" => Some(Self::User),
            "channel" => Some(Self::Channel),
            "role" => Some(Self::Role),
            "mentionable" => Some(Self::Mentionable),
            _ => None,
        }
    }

    /// Discord `ApplicationCommandOptionType` wire value.
    pub fn wire_type(self) -> u8 {
        match self {
            Self::String => 3,
            Self::Integer => 4,
            Self::Boolean => 5,
            Self::User => 6,
            Self::Channel => 7,
            Self::Role => 8,
            Self::Mentionable => 9,
            Self::Number => 10,
        }
    }

    /// Choices and min/max bounds apply only to string/integer/number options.
    fn is_scalar(self) -> bool {
        matches!(self, Self::String | Self::Integer | Self::Number)
    }
}

/// A predefined choice for a scalar option. `value` is held as text and coerced
/// to the option's wire type when serialized (Discord requires integer/number
/// choice values to be numeric).
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub name: String,
    pub value: String,
}

/// A typed slash-command option in Discord's registration shape.
#[derive(Debug, Clone, PartialEq)]
pub struct OptionSpec {
    pub name: String,
    pub description: String,
    pub kind: OptKind,
    pub required: bool,
    pub choices: Vec<Choice>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub min_length: Option<u32>,
    pub max_length: Option<u32>,
}

impl OptionSpec {
    /// Serialize to a Discord application-command option object. Choices and
    /// bounds are emitted only for the kinds Discord accepts them on.
    pub fn to_registration_json(&self) -> Value {
        let mut obj = Map::new();
        obj.insert("name".to_string(), json!(self.name));
        obj.insert("description".to_string(), json!(self.description));
        obj.insert("type".to_string(), json!(self.kind.wire_type()));
        obj.insert("required".to_string(), json!(self.required));

        if self.kind.is_scalar() && !self.choices.is_empty() {
            let choices: Vec<Value> = self
                .choices
                .iter()
                .map(|c| json!({ "name": c.name, "value": coerce_value(&c.value, self.kind) }))
                .collect();
            obj.insert("choices".to_string(), Value::Array(choices));
        }

        match self.kind {
            OptKind::Integer | OptKind::Number => {
                if let Some(min) = self.min {
                    obj.insert("min_value".to_string(), number_value(min, self.kind));
                }
                if let Some(max) = self.max {
                    obj.insert("max_value".to_string(), number_value(max, self.kind));
                }
            }
            OptKind::String => {
                if let Some(min) = self.min_length {
                    obj.insert("min_length".to_string(), json!(min));
                }
                if let Some(max) = self.max_length {
                    obj.insert("max_length".to_string(), json!(max));
                }
            }
            _ => {}
        }
        Value::Object(obj)
    }
}

/// Coerce a textual choice value to the option's wire type, falling back to the
/// string form when it doesn't parse as the numeric type.
fn coerce_value(value: &str, kind: OptKind) -> Value {
    match kind {
        OptKind::Integer => value
            .parse::<i64>()
            .map(|n| json!(n))
            .unwrap_or_else(|_| json!(value)),
        OptKind::Number => value
            .parse::<f64>()
            .map(|n| json!(n))
            .unwrap_or_else(|_| json!(value)),
        _ => json!(value),
    }
}

fn number_value(v: f64, kind: OptKind) -> Value {
    match kind {
        OptKind::Integer => json!(v as i64),
        _ => json!(v),
    }
}

/// Extract the values a user submitted for a slash command's options out of an
/// INTERACTION_CREATE payload's `data.options[]`, as `(name, display)` pairs in
/// the order Discord sent them. The value is stringified by JSON kind (string
/// as-is; number/bool to text) for folding into the synthesized agent prompt.
/// This generalises the single-`input` extractor for typed commands.
///
/// Limitation: user/channel/role/mentionable options yield the raw snowflake id
/// (Discord puts the resolved entity in `data.resolved`, which is not consulted
/// here) — resolving ids to display names/mentions is a follow-on.
pub fn extract_submitted_options(data: &Value) -> Vec<(String, String)> {
    data.get("data")
        .and_then(|d| d.get("options"))
        .and_then(|o| o.as_array())
        .map(|opts| {
            opts.iter()
                .filter_map(|o| {
                    let name = o.get("name")?.as_str()?.to_string();
                    let value = o.get("value").map(stringify_value).unwrap_or_default();
                    Some((name, value))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn stringify_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opt(name: &str, kind: OptKind, required: bool) -> OptionSpec {
        OptionSpec {
            name: name.to_string(),
            description: format!("{name} option"),
            kind,
            required,
            choices: Vec::new(),
            min: None,
            max: None,
            min_length: None,
            max_length: None,
        }
    }

    #[test]
    fn manifest_kinds_map_to_wire_types() {
        assert_eq!(OptKind::from_manifest("string").unwrap().wire_type(), 3);
        assert_eq!(OptKind::from_manifest("INT").unwrap().wire_type(), 4);
        assert_eq!(OptKind::from_manifest("boolean").unwrap().wire_type(), 5);
        assert_eq!(OptKind::from_manifest("number").unwrap().wire_type(), 10);
        assert_eq!(
            OptKind::from_manifest("mentionable").unwrap().wire_type(),
            9
        );
        assert!(OptKind::from_manifest("nonsense").is_none());
    }

    #[test]
    fn a_plain_required_string_serializes_minimally() {
        assert_eq!(
            opt("input", OptKind::String, true).to_registration_json(),
            json!({ "name": "input", "description": "input option", "type": 3, "required": true })
        );
    }

    #[test]
    fn integer_bounds_emit_numeric_min_max() {
        let mut o = opt("limit", OptKind::Integer, false);
        o.min = Some(1.0);
        o.max = Some(50.0);
        assert_eq!(
            o.to_registration_json(),
            json!({
                "name": "limit", "description": "limit option", "type": 4, "required": false,
                "min_value": 1, "max_value": 50
            })
        );
    }

    #[test]
    fn string_length_bounds_emit_min_max_length() {
        let mut o = opt("query", OptKind::String, true);
        o.min_length = Some(2);
        o.max_length = Some(200);
        let j = o.to_registration_json();
        assert_eq!(j["min_length"], json!(2));
        assert_eq!(j["max_length"], json!(200));
        assert!(j.get("min_value").is_none());
    }

    #[test]
    fn choices_coerce_to_the_option_type_and_only_on_scalars() {
        let mut s = opt("sort", OptKind::String, false);
        s.choices = vec![Choice {
            name: "Newest".to_string(),
            value: "new".to_string(),
        }];
        assert_eq!(
            s.to_registration_json()["choices"],
            json!([{ "name": "Newest", "value": "new" }])
        );

        let mut i = opt("count", OptKind::Integer, false);
        i.choices = vec![Choice {
            name: "Ten".to_string(),
            value: "10".to_string(),
        }];
        // integer choice value coerces to a JSON number
        assert_eq!(
            i.to_registration_json()["choices"],
            json!([{ "name": "Ten", "value": 10 }])
        );

        // a non-scalar kind never emits choices even if some were set
        let mut u = opt("who", OptKind::User, false);
        u.choices = vec![Choice {
            name: "x".to_string(),
            value: "y".to_string(),
        }];
        assert!(u.to_registration_json().get("choices").is_none());
    }

    #[test]
    fn extract_submitted_reads_typed_values_in_order_and_stringifies() {
        let interaction = json!({
            "type": 2,
            "data": {
                "name": "search",
                "options": [
                    { "name": "query", "type": 3, "value": "rust" },
                    { "name": "limit", "type": 4, "value": 5 },
                    { "name": "verbose", "type": 5, "value": true }
                ]
            }
        });
        assert_eq!(
            extract_submitted_options(&interaction),
            vec![
                ("query".to_string(), "rust".to_string()),
                ("limit".to_string(), "5".to_string()),
                ("verbose".to_string(), "true".to_string()),
            ]
        );
    }

    #[test]
    fn extract_submitted_is_empty_when_no_options() {
        assert!(extract_submitted_options(&json!({ "data": { "name": "x" } })).is_empty());
        assert!(extract_submitted_options(&json!({})).is_empty());
    }
}
