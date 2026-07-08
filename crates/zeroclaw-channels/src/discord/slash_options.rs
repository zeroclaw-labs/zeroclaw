//! Typed slash-command option model (contract tier): the option shapes that
//! flow from a skill's `[[skill.slash_options]]` manifest declaration into the
//! Discord application-command registration body. Pure data plus the trivial
//! serialization to Discord's option JSON — no IO, no runtime types. Imported
//! by `types` (the command spec carries a `Vec<OptionSpec>`) and by `slash`
//! (which maps skill declarations into these and builds the registration body);
//! imports no sibling impl module, so the contract layer stays acyclic.

use serde_json::{Map, Value, json};

/// Discord caps a registered option's static `choices` array — and an
/// autocomplete answer's `choices` — at 25. A scalar option whose predefined
/// list exceeds this can't be registered as static choices (Discord 400s); it
/// is instead flagged `autocomplete: true` and its choices are served (filtered
/// by the user's partial input) through the type-4 dispatch arm. The same cap
/// truncates the answered set.
pub(crate) const DISCORD_MAX_STATIC_CHOICES: usize = 25;

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
    /// The Discord wire projection of a canonical [`SlashOptionKind`]. Exhaustive
    /// so a new canonical variant forces a wire mapping here rather than silently
    /// dropping.
    pub fn from_kind(kind: zeroclaw_runtime::skills::SlashOptionKind) -> Self {
        use zeroclaw_runtime::skills::SlashOptionKind as K;
        match kind {
            K::String => Self::String,
            K::Integer => Self::Integer,
            K::Number => Self::Number,
            K::Boolean => Self::Boolean,
            K::User => Self::User,
            K::Channel => Self::Channel,
            K::Role => Self::Role,
            K::Mentionable => Self::Mentionable,
        }
    }

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
    /// Discord-locale-keyed translations of `description` (from the skill
    /// manifest, filtered to supported locale codes). Empty → no
    /// `description_localizations` key is registered for this option.
    pub description_localizations: std::collections::BTreeMap<String, String>,
    pub kind: OptKind,
    pub required: bool,
    pub choices: Vec<Choice>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub min_length: Option<u32>,
    pub max_length: Option<u32>,
}

impl OptionSpec {
    /// Whether this option is served via autocomplete (type-4) rather than a
    /// static `choices` array. Discord forbids both on one option and rejects a
    /// static list over [`DISCORD_MAX_STATIC_CHOICES`]; so a scalar option whose
    /// predefined list exceeds that cap is registered `autocomplete: true` and
    /// its choices are filtered + answered through the type-4 dispatch arm. A
    /// small list (≤ cap) stays static — Discord renders it natively without
    /// firing autocomplete, so marking it would only lose that native rendering.
    pub(crate) fn serves_autocomplete(&self) -> bool {
        self.kind.is_scalar() && self.choices.len() > DISCORD_MAX_STATIC_CHOICES
    }

    /// The predefined choices matching a user's partial input, as
    /// `(name, value)` pairs, capped at [`DISCORD_MAX_STATIC_CHOICES`]. The
    /// filter is a case-insensitive substring match on the choice name (and,
    /// for a non-empty partial, the value too), so a few keystrokes narrow a
    /// large list. An empty partial returns the first `cap` choices. A
    /// non-autocomplete option (no choices, or a small static list) returns
    /// empty — only options flagged autocomplete receive type-4 events.
    pub(crate) fn matching_choices(&self, partial: &str) -> Vec<(String, String)> {
        if !self.serves_autocomplete() {
            return Vec::new();
        }
        let needle = partial.trim().to_ascii_lowercase();
        self.choices
            .iter()
            .filter(|c| {
                needle.is_empty()
                    || c.name.to_ascii_lowercase().contains(&needle)
                    || c.value.to_ascii_lowercase().contains(&needle)
            })
            .take(DISCORD_MAX_STATIC_CHOICES)
            .map(|c| (c.name.clone(), c.value.clone()))
            .collect()
    }

    /// Serialize to a Discord application-command option object. Choices and
    /// bounds are emitted only for the kinds Discord accepts them on.
    pub fn to_registration_json(&self) -> Value {
        let mut obj = Map::new();
        obj.insert("name".to_string(), json!(self.name));
        obj.insert("description".to_string(), json!(self.description));
        if !self.description_localizations.is_empty() {
            obj.insert(
                "description_localizations".to_string(),
                json!(self.description_localizations),
            );
        }
        obj.insert("type".to_string(), json!(self.kind.wire_type()));
        obj.insert("required".to_string(), json!(self.required));

        if self.serves_autocomplete() {
            // Over Discord's static-choice cap: flag autocomplete and serve the
            // choices through the type-4 arm. Discord rejects a static `choices`
            // array alongside `autocomplete: true`, so it is intentionally omitted.
            obj.insert("autocomplete".to_string(), json!(true));
        } else if self.kind.is_scalar() && !self.choices.is_empty() {
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

/// The focused option of an APPLICATION_COMMAND_AUTOCOMPLETE (type-4) payload,
/// as `(command_name, option_name, partial_input)`. Discord marks exactly one
/// `data.options[]` entry `"focused": true` and carries the user's partial text
/// in its `value`. Returns `None` when no option is focused or the command name
/// is absent (a malformed payload we simply can't complete). The partial is
/// stringified (a focused integer/number option carries a numeric `value`).
pub(crate) fn extract_focused_option(data: &Value) -> Option<(String, String, String)> {
    let d = data.get("data")?;
    let command = d.get("name")?.as_str()?.to_string();
    let focused = d
        .get("options")
        .and_then(|o| o.as_array())?
        .iter()
        .find(|o| o.get("focused").and_then(Value::as_bool).unwrap_or(false))?;
    let option_name = focused.get("name")?.as_str()?.to_string();
    let partial = focused
        .get("value")
        .map(stringify_value)
        .unwrap_or_default();
    Some((command, option_name, partial))
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
            description_localizations: Default::default(),
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

    fn opt_with_choices(name: &str, kind: OptKind, n: usize) -> OptionSpec {
        let mut o = opt(name, kind, false);
        o.choices = (0..n)
            .map(|i| Choice {
                name: format!("choice-{i:02}"),
                value: format!("v{i:02}"),
            })
            .collect();
        o
    }

    #[test]
    fn serves_autocomplete_only_for_scalars_over_the_static_cap() {
        // ≤ 25 stays static (Discord renders it natively).
        assert!(
            !opt_with_choices("a", OptKind::String, DISCORD_MAX_STATIC_CHOICES)
                .serves_autocomplete()
        );
        // > 25 must be autocomplete (a static list that big would 400).
        assert!(
            opt_with_choices("a", OptKind::String, DISCORD_MAX_STATIC_CHOICES + 1)
                .serves_autocomplete()
        );
        assert!(opt_with_choices("a", OptKind::Integer, 30).serves_autocomplete());
        // Non-scalar kinds never carry choices / autocomplete.
        assert!(!opt_with_choices("a", OptKind::User, 30).serves_autocomplete());
        // No choices → nothing to serve.
        assert!(!opt("a", OptKind::String, false).serves_autocomplete());
    }

    #[test]
    fn registration_flags_autocomplete_and_omits_static_choices_over_cap() {
        let big = opt_with_choices("sort", OptKind::String, 40);
        let j = big.to_registration_json();
        assert_eq!(j["autocomplete"], json!(true));
        assert!(
            j.get("choices").is_none(),
            "static choices omitted when autocomplete"
        );

        // A small list keeps the static `choices` array and no autocomplete flag.
        let small = opt_with_choices("sort", OptKind::String, 5);
        let j = small.to_registration_json();
        assert!(j.get("autocomplete").is_none());
        assert_eq!(j["choices"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn matching_choices_filters_by_partial_substring_case_insensitively() {
        let o = opt_with_choices("sort", OptKind::String, 40);
        // "choice-07" matches name; "v07" matches value — substring, any case.
        let m = o.matching_choices("CHOICE-07");
        assert_eq!(m, vec![("choice-07".to_string(), "v07".to_string())]);
        let m = o.matching_choices("v07");
        assert_eq!(m, vec![("choice-07".to_string(), "v07".to_string())]);
    }

    #[test]
    fn matching_choices_caps_at_25_and_empty_partial_returns_first_cap() {
        let o = opt_with_choices("sort", OptKind::String, 40);
        let m = o.matching_choices("");
        assert_eq!(
            m.len(),
            DISCORD_MAX_STATIC_CHOICES,
            "empty partial → first cap"
        );
        // "choice-" matches all 40 but the answer is capped to 25.
        let m = o.matching_choices("choice-");
        assert_eq!(m.len(), DISCORD_MAX_STATIC_CHOICES);
    }

    #[test]
    fn matching_choices_is_empty_for_no_match_and_for_non_autocomplete_options() {
        let big = opt_with_choices("sort", OptKind::String, 40);
        assert!(
            big.matching_choices("zzz-nope").is_empty(),
            "no substring match → empty"
        );
        // A small static list is not served via autocomplete at all.
        let small = opt_with_choices("sort", OptKind::String, 5);
        assert!(small.matching_choices("choice").is_empty());
        // No choices.
        assert!(
            opt("q", OptKind::String, false)
                .matching_choices("a")
                .is_empty()
        );
    }

    #[test]
    fn extract_focused_option_reads_command_option_and_partial() {
        let payload = json!({
            "type": 4,
            "data": {
                "name": "search",
                "options": [
                    { "name": "scope", "type": 3, "value": "repo" },
                    { "name": "query", "type": 3, "value": "rus", "focused": true }
                ]
            }
        });
        assert_eq!(
            extract_focused_option(&payload),
            Some(("search".to_string(), "query".to_string(), "rus".to_string()))
        );
    }

    #[test]
    fn extract_focused_option_handles_numeric_partial_and_none() {
        // A focused integer option carries a numeric value → stringified.
        let numeric = json!({
            "data": { "name": "c", "options": [ { "name": "limit", "type": 4, "value": 12, "focused": true } ] }
        });
        assert_eq!(
            extract_focused_option(&numeric),
            Some(("c".to_string(), "limit".to_string(), "12".to_string()))
        );
        // No option marked focused → None.
        let unfocused = json!({
            "data": { "name": "c", "options": [ { "name": "limit", "type": 4, "value": 12 } ] }
        });
        assert!(extract_focused_option(&unfocused).is_none());
        // Missing command name → None.
        assert!(extract_focused_option(&json!({ "data": {} })).is_none());
    }

    #[test]
    fn from_kind_projects_every_canonical_variant_to_matching_wire_type() {
        use zeroclaw_runtime::skills::SlashOptionKind as K;
        // Walk the canonical registry: each variant projects to a wire OptKind
        // whose manifest name round-trips back to the same canonical token, and
        // whose capability flags agree with the canonical enum. Exhaustive by
        // construction (walks `K::ALL`), so a new canonical variant that lacks a
        // wire projection fails to compile in `from_kind` before it reaches here.
        for kind in K::ALL {
            let projected = OptKind::from_kind(kind);
            assert_eq!(
                OptKind::from_manifest(kind.manifest_name()),
                Some(projected),
                "manifest token {:?} must parse back to the projected wire kind",
                kind.manifest_name(),
            );
            assert_eq!(
                projected.is_scalar(),
                kind.supports_choices(),
                "choice capability disagreement for {:?}",
                kind.manifest_name(),
            );
        }
    }
}
