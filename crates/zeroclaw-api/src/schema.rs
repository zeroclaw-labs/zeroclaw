//! JSON Schema cleaning and validation for LLM tool-calling compatibility.
//!
//! Different model_providers support different subsets of JSON Schema. This module
//! normalizes tool schemas to improve cross-provider compatibility while
//! preserving semantic intent.
//!
//! ## What this module does
//!
//! 1. Removes unsupported keywords per model_provider strategy
//! 2. Resolves local `$ref` entries from `$defs` and `definitions`
//! 3. Flattens literal `anyOf` / `oneOf` unions into `enum`
//! 4. Strips nullable variants from unions and `type` arrays
//! 5. Converts `const` to single-value `enum`
//! 6. Detects circular references and stops recursion safely
//!
//! # Example
//!
//! ```rust
//! use serde_json::json;
//! use zeroclaw_api::schema::SchemaCleanr;
//!
//! let dirty_schema = json!({
//!     "type": "object",
//!     "properties": {
//!         "name": {
//!             "type": "string",
//!             "minLength": 1, // Gemini rejects this
//!             "pattern": "^[a-z]+$" // Gemini rejects this
//!         },
//!         "age": {
//!             "$ref": "#/$defs/Age" // Needs resolution
//!         }
//!     },
//!     "$defs": {
//!         "Age": {
//!             "type": "integer",
//!             "minimum": 0 // Gemini rejects this
//!         }
//!     }
//! });
//!
//! let cleaned = SchemaCleanr::clean_for_gemini(dirty_schema);
//!
//! // Result:
//! // {
//! // "type": "object",
//! // "properties": {
//! // "name": { "type": "string" },
//! // "age": { "type": "integer" }
//! // }
//! // }
//! ```
//!
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Keywords that Gemini rejects for tool schemas.
pub const GEMINI_UNSUPPORTED_KEYWORDS: &[&str] = &[
    // Schema composition
    "$ref",
    "$schema",
    "$id",
    "$defs",
    "definitions",
    // Property constraints
    "additionalProperties",
    "patternProperties",
    // String constraints
    "minLength",
    "maxLength",
    "pattern",
    "format",
    // Number constraints
    "minimum",
    "maximum",
    "multipleOf",
    // Array constraints
    "minItems",
    "maxItems",
    "uniqueItems",
    // Object constraints
    "minProperties",
    "maxProperties",
    // Non-standard
    "examples", // OpenAPI keyword, not JSON Schema
];

/// Keywords that should be preserved during cleaning (metadata).
const SCHEMA_META_KEYS: &[&str] = &["description", "title", "default"];

/// Schema cleaning strategies for different LLM model_providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CleaningStrategy {
    /// Gemini (Google AI / Vertex AI) - Most restrictive
    Gemini,
    /// Anthropic Claude - Moderately permissive
    Anthropic,
    /// OpenAI GPT - Most permissive
    OpenAI,
    /// Conservative: Remove only universally unsupported keywords
    Conservative,
}

impl CleaningStrategy {
    /// Get the list of unsupported keywords for this strategy.
    pub fn unsupported_keywords(self) -> &'static [&'static str] {
        match self {
            Self::Gemini => GEMINI_UNSUPPORTED_KEYWORDS,
            Self::Anthropic => &["$ref", "$defs", "definitions"], // Anthropic doesn't resolve refs
            Self::OpenAI => &[],                                  // OpenAI is most permissive
            Self::Conservative => &["$ref", "$defs", "definitions", "additionalProperties"],
        }
    }
}

/// JSON Schema cleaner optimized for LLM tool calling.
pub struct SchemaCleanr;

impl SchemaCleanr {
    /// Clean schema for Gemini compatibility (strictest).
    ///
    /// This is the most aggressive cleaning strategy, removing all keywords
    /// that Gemini's API rejects.
    pub fn clean_for_gemini(schema: Value) -> Value {
        Self::clean(schema, CleaningStrategy::Gemini)
    }

    /// Clean schema for Anthropic compatibility.
    pub fn clean_for_anthropic(schema: Value) -> Value {
        Self::clean(schema, CleaningStrategy::Anthropic)
    }

    /// Clean schema for OpenAI compatibility (most permissive).
    pub fn clean_for_openai(schema: Value) -> Value {
        Self::clean(schema, CleaningStrategy::OpenAI)
    }

    /// Zero-copy wrapper around [`Self::clean`] for `Arc`-shared tool schemas:
    /// returns the same `Arc` when the pre-scan proves cleaning is a
    /// no-op, deep-copying the tree only when a rewrite is actually needed.
    pub fn clean_shared(schema: &Arc<Value>, strategy: CleaningStrategy) -> Arc<Value> {
        if Self::needs_cleaning(schema, strategy) {
            Arc::new(Self::clean((**schema).clone(), strategy))
        } else {
            Arc::clone(schema)
        }
    }

    /// Conservative read-only pre-scan: `true` when [`Self::clean`] with
    /// `strategy` could change `schema`.
    ///
    /// False positives are allowed (a flagged schema may clean to an equal
    /// value); false negatives are not — `!needs_cleaning(s)` must imply
    /// `clean(s) == s`. The triggers mirror every rewrite path in
    /// `clean_object`: strategy-specific keyword removal, plus the
    /// strategy-independent rewrites (`$ref` resolution, `const` → `enum`,
    /// `anyOf`/`oneOf` simplification and sibling-`type` skipping, and
    /// null-stripping in `type` arrays).
    pub fn needs_cleaning(schema: &Value, strategy: CleaningStrategy) -> bool {
        match schema {
            Value::Object(obj) => {
                let unsupported = strategy.unsupported_keywords();
                for (key, value) in obj {
                    if unsupported.contains(&key.as_str()) {
                        return true;
                    }
                    match key.as_str() {
                        "$ref" | "const" | "anyOf" | "oneOf" => return true,
                        "type" if value.is_array() => return true,
                        _ => {}
                    }
                    if Self::needs_cleaning(value, strategy) {
                        return true;
                    }
                }
                false
            }
            Value::Array(arr) => arr.iter().any(|v| Self::needs_cleaning(v, strategy)),
            _ => false,
        }
    }

    /// Clean schema with specified strategy.
    pub fn clean(schema: Value, strategy: CleaningStrategy) -> Value {
        // Extract $defs for reference resolution
        let defs = if let Some(obj) = schema.as_object() {
            Self::extract_defs(obj)
        } else {
            HashMap::new()
        };

        Self::clean_with_defs(schema, &defs, strategy, &mut HashSet::new())
    }

    /// Validate that a schema is suitable for LLM tool calling.
    ///
    /// Returns an error if the schema is invalid or missing required fields.
    pub fn validate(schema: &Value) -> anyhow::Result<()> {
        let obj = schema
            .as_object()
            .ok_or_else(|| anyhow::Error::msg("Schema must be an object"))?;

        // Must have 'type' field
        if !obj.contains_key("type") {
            anyhow::bail!("Schema missing required 'type' field");
        }

        // If type is 'object', should have 'properties'
        if let Some(Value::String(t)) = obj.get("type")
            && t == "object"
            && !obj.contains_key("properties")
        {
            eprintln!("warn: Object schema without 'properties' field may cause issues");
        }

        Ok(())
    }

    // --------------------------------------------------------------------
    // Internal implementation
    // --------------------------------------------------------------------

    /// Extract $defs and definitions into a flat map for reference resolution.
    fn extract_defs(obj: &Map<String, Value>) -> HashMap<String, Value> {
        let mut defs = HashMap::new();

        // Extract from $defs (JSON Schema 2019-09+)
        if let Some(Value::Object(defs_obj)) = obj.get("$defs") {
            for (key, value) in defs_obj {
                defs.insert(key.clone(), value.clone());
            }
        }

        // Extract from definitions (JSON Schema draft-07)
        if let Some(Value::Object(defs_obj)) = obj.get("definitions") {
            for (key, value) in defs_obj {
                defs.insert(key.clone(), value.clone());
            }
        }

        defs
    }

    /// Recursively clean a schema value.
    fn clean_with_defs(
        schema: Value,
        defs: &HashMap<String, Value>,
        strategy: CleaningStrategy,
        ref_stack: &mut HashSet<String>,
    ) -> Value {
        match schema {
            Value::Object(obj) => Self::clean_object(obj, defs, strategy, ref_stack),
            Value::Array(arr) => Value::Array(
                arr.into_iter()
                    .map(|v| Self::clean_with_defs(v, defs, strategy, ref_stack))
                    .collect(),
            ),
            other => other,
        }
    }

    /// Clean an object schema.
    fn clean_object(
        obj: Map<String, Value>,
        defs: &HashMap<String, Value>,
        strategy: CleaningStrategy,
        ref_stack: &mut HashSet<String>,
    ) -> Value {
        // Handle $ref resolution
        if let Some(Value::String(ref_value)) = obj.get("$ref") {
            return Self::resolve_ref(ref_value, &obj, defs, strategy, ref_stack);
        }

        // Handle anyOf/oneOf simplification
        if (obj.contains_key("anyOf") || obj.contains_key("oneOf"))
            && let Some(simplified) = Self::try_simplify_union(&obj, defs, strategy, ref_stack)
        {
            return simplified;
        }

        // Build cleaned object
        let mut cleaned = Map::new();
        let unsupported: HashSet<&str> = strategy.unsupported_keywords().iter().copied().collect();
        let has_union = obj.contains_key("anyOf") || obj.contains_key("oneOf");

        for (key, value) in obj {
            // Skip unsupported keywords
            if unsupported.contains(key.as_str()) {
                continue;
            }

            // Special handling for specific keys
            match key.as_str() {
                // Convert const to enum
                "const" => {
                    cleaned.insert("enum".to_string(), json!([value]));
                }
                // Skip type if we have anyOf/oneOf (they define the type)
                "type" if has_union => {
                    // Skip
                }
                // Handle type arrays (remove null)
                "type" if matches!(value, Value::Array(_)) => {
                    let cleaned_value = Self::clean_type_array(value);
                    cleaned.insert(key, cleaned_value);
                }
                // Recursively clean nested schemas
                "properties" => {
                    let cleaned_value = Self::clean_properties(value, defs, strategy, ref_stack);
                    cleaned.insert(key, cleaned_value);
                }
                "items" => {
                    let cleaned_value = Self::clean_with_defs(value, defs, strategy, ref_stack);
                    cleaned.insert(key, cleaned_value);
                }
                "anyOf" | "oneOf" | "allOf" => {
                    let cleaned_value = Self::clean_union(value, defs, strategy, ref_stack);
                    cleaned.insert(key, cleaned_value);
                }
                // Keep all other keys, cleaning nested objects/arrays recursively.
                _ => {
                    let cleaned_value = match value {
                        Value::Object(_) | Value::Array(_) => {
                            Self::clean_with_defs(value, defs, strategy, ref_stack)
                        }
                        other => other,
                    };
                    cleaned.insert(key, cleaned_value);
                }
            }
        }

        Value::Object(cleaned)
    }

    /// Resolve a $ref to its definition.
    fn resolve_ref(
        ref_value: &str,
        obj: &Map<String, Value>,
        defs: &HashMap<String, Value>,
        strategy: CleaningStrategy,
        ref_stack: &mut HashSet<String>,
    ) -> Value {
        // Prevent circular references
        if ref_stack.contains(ref_value) {
            eprintln!("warn: Circular $ref detected: {}", ref_value);
            return Self::preserve_meta(obj, Value::Object(Map::new()));
        }

        // Try to resolve local ref (#/$defs/Name or #/definitions/Name)
        if let Some(def_name) = Self::parse_local_ref(ref_value)
            && let Some(definition) = defs.get(def_name.as_str())
        {
            ref_stack.insert(ref_value.to_string());
            let cleaned = Self::clean_with_defs(definition.clone(), defs, strategy, ref_stack);
            ref_stack.remove(ref_value);
            return Self::preserve_meta(obj, cleaned);
        }

        // Can't resolve: return empty object with metadata
        eprintln!("warn: Cannot resolve $ref: {}", ref_value);
        Self::preserve_meta(obj, Value::Object(Map::new()))
    }

    /// Parse a local JSON Pointer ref (#/$defs/Name).
    fn parse_local_ref(ref_value: &str) -> Option<String> {
        ref_value
            .strip_prefix("#/$defs/")
            .or_else(|| ref_value.strip_prefix("#/definitions/"))
            .map(Self::decode_json_pointer)
    }

    /// Decode JSON Pointer escaping (`~0` = `~`, `~1` = `/`).
    fn decode_json_pointer(segment: &str) -> String {
        if !segment.contains('~') {
            return segment.to_string();
        }

        let mut decoded = String::with_capacity(segment.len());
        let mut chars = segment.chars().peekable();

        while let Some(ch) = chars.next() {
            if ch == '~' {
                match chars.peek().copied() {
                    Some('0') => {
                        chars.next();
                        decoded.push('~');
                    }
                    Some('1') => {
                        chars.next();
                        decoded.push('/');
                    }
                    _ => decoded.push('~'),
                }
            } else {
                decoded.push(ch);
            }
        }

        decoded
    }

    /// Try to simplify anyOf/oneOf to a simpler form.
    fn try_simplify_union(
        obj: &Map<String, Value>,
        defs: &HashMap<String, Value>,
        strategy: CleaningStrategy,
        ref_stack: &mut HashSet<String>,
    ) -> Option<Value> {
        let union_key = if obj.contains_key("anyOf") {
            "anyOf"
        } else if obj.contains_key("oneOf") {
            "oneOf"
        } else {
            return None;
        };

        let variants = obj.get(union_key)?.as_array()?;

        // Clean all variants first
        let cleaned_variants: Vec<Value> = variants
            .iter()
            .map(|v| Self::clean_with_defs(v.clone(), defs, strategy, ref_stack))
            .collect();

        // Strip null variants
        let non_null: Vec<Value> = cleaned_variants
            .into_iter()
            .filter(|v| !Self::is_null_schema(v))
            .collect();

        // If only one variant remains after stripping nulls, return it
        if non_null.len() == 1 {
            return Some(Self::preserve_meta(obj, non_null[0].clone()));
        }

        // Try to flatten to enum if all variants are literals
        if let Some(enum_value) = Self::try_flatten_literal_union(&non_null) {
            return Some(Self::preserve_meta(obj, enum_value));
        }

        None
    }

    /// Check if a schema represents null type.
    fn is_null_schema(value: &Value) -> bool {
        if let Some(obj) = value.as_object() {
            // { const: null }
            if let Some(Value::Null) = obj.get("const") {
                return true;
            }
            // { enum: [null] }
            if let Some(Value::Array(arr)) = obj.get("enum")
                && arr.len() == 1
                && matches!(arr[0], Value::Null)
            {
                return true;
            }
            // { type: "null" }
            if let Some(Value::String(t)) = obj.get("type")
                && t == "null"
            {
                return true;
            }
        }
        false
    }

    /// Try to flatten anyOf/oneOf with only literal values to enum.
    ///
    /// Example: `anyOf: [{const: "a"}, {const: "b"}]` -> `{type: "string", enum: ["a", "b"]}`
    fn try_flatten_literal_union(variants: &[Value]) -> Option<Value> {
        if variants.is_empty() {
            return None;
        }

        let mut all_values = Vec::new();
        let mut common_type: Option<String> = None;

        for variant in variants {
            let obj = variant.as_object()?;

            // Extract literal value from const or single-item enum
            let literal_value = if let Some(const_val) = obj.get("const") {
                const_val.clone()
            } else if let Some(Value::Array(arr)) = obj.get("enum") {
                if arr.len() == 1 {
                    arr[0].clone()
                } else {
                    return None;
                }
            } else {
                return None;
            };

            // Check type consistency
            let variant_type = obj.get("type")?.as_str()?;
            match &common_type {
                None => common_type = Some(variant_type.to_string()),
                Some(t) if t != variant_type => return None,
                _ => {}
            }

            all_values.push(literal_value);
        }

        common_type.map(|t| {
            json!({
                "type": t,
                "enum": all_values
            })
        })
    }

    /// Clean type array, removing null.
    fn clean_type_array(value: Value) -> Value {
        if let Value::Array(types) = value {
            let non_null: Vec<Value> = types
                .into_iter()
                .filter(|v| v.as_str() != Some("null"))
                .collect();

            match non_null.len() {
                0 => Value::String("null".to_string()),
                1 => non_null
                    .into_iter()
                    .next()
                    .unwrap_or(Value::String("null".to_string())),
                _ => Value::Array(non_null),
            }
        } else {
            value
        }
    }

    /// Clean properties object.
    fn clean_properties(
        value: Value,
        defs: &HashMap<String, Value>,
        strategy: CleaningStrategy,
        ref_stack: &mut HashSet<String>,
    ) -> Value {
        if let Value::Object(props) = value {
            let cleaned: Map<String, Value> = props
                .into_iter()
                .map(|(k, v)| (k, Self::clean_with_defs(v, defs, strategy, ref_stack)))
                .collect();
            Value::Object(cleaned)
        } else {
            value
        }
    }

    /// Clean union (anyOf/oneOf/allOf).
    fn clean_union(
        value: Value,
        defs: &HashMap<String, Value>,
        strategy: CleaningStrategy,
        ref_stack: &mut HashSet<String>,
    ) -> Value {
        if let Value::Array(variants) = value {
            let cleaned: Vec<Value> = variants
                .into_iter()
                .map(|v| Self::clean_with_defs(v, defs, strategy, ref_stack))
                .collect();
            Value::Array(cleaned)
        } else {
            value
        }
    }

    /// Preserve metadata (description, title, default) from source to target.
    fn preserve_meta(source: &Map<String, Value>, mut target: Value) -> Value {
        if let Value::Object(target_obj) = &mut target {
            for &key in SCHEMA_META_KEYS {
                if let Some(value) = source.get(key) {
                    target_obj.insert(key.to_string(), value.clone());
                }
            }
        }
        target
    }
}

/// Upper bound on retained [`SchemaCleanCache`] entries. Sized for the
/// realistic ceiling of registered tools × strategies; overflow first drops
/// entries whose source schema is gone, then falls back to a full clear.
const SCHEMA_CLEAN_CACHE_CAP: usize = 512;

struct SchemaCleanCacheEntry {
    /// Identity of the source schema this result was cleaned from. `Weak`
    /// so a cache entry never keeps a replaced (e.g. MCP-reconnect) schema
    /// alive on its own.
    source: std::sync::Weak<Value>,
    cleaned: Arc<Value>,
}

/// Bounded memo of [`SchemaCleanr::clean_shared`] results, keyed by source
/// schema identity and strategy.
///
/// Cleaning is a pure function of `(schema, strategy)`, but tool schemas
/// that need rewriting (`$ref`/`$defs`, `const`, unions — pervasive in
/// generated MCP schemas) would otherwise be deep-copied on every provider
/// request. Providers that clean per request embed one of these so each
/// distinct schema is cleaned once per strategy for as long as it stays
/// registered. This holds no canonical state: entries are derived values,
/// keyed by the identity of the canonical `Arc` the tool registry owns, and
/// the memoized result is byte-stable across requests (which also keeps
/// provider-side prompt caching stable).
///
/// Only *rewritten* results are cached. A no-op clean is returned straight
/// from the pre-scan and never inserted: such an entry's `cleaned` field
/// would be the very allocation its `source` `Weak` watches, pinning it
/// forever (the dead-entry prune could never fire), and ephemeral per-call
/// `Arc`s — the default `Tool::spec()` builds a fresh one every iteration —
/// would flood the map until the overflow clear evicted the live memos this
/// cache exists to keep.
///
/// A hit requires upgrading the stored `Weak` **and** `Arc::ptr_eq` with
/// the candidate. Stale hits are impossible twice over: while an entry
/// lives, its `Weak` keeps the source `ArcInner` allocation reserved, so no
/// new schema can occupy that address; and once the source is dropped the
/// `Weak` permanently refuses to upgrade, so the entry can only miss.
pub struct SchemaCleanCache {
    entries: std::sync::Mutex<HashMap<(usize, CleaningStrategy), SchemaCleanCacheEntry>>,
}

impl Default for SchemaCleanCache {
    fn default() -> Self {
        Self::new()
    }
}

impl SchemaCleanCache {
    pub fn new() -> Self {
        Self {
            entries: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Memoized [`SchemaCleanr::clean_shared`]: returns the shared source
    /// `Arc` when cleaning is a no-op, and otherwise the cleaned tree —
    /// deep-computed at most once per (live schema, strategy) pair.
    pub fn clean_shared(&self, schema: &Arc<Value>, strategy: CleaningStrategy) -> Arc<Value> {
        let key = (Arc::as_ptr(schema) as usize, strategy);
        {
            let entries = self
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(entry) = entries.get(&key)
                && let Some(live_source) = entry.source.upgrade()
                && Arc::ptr_eq(&live_source, schema)
            {
                return Arc::clone(&entry.cleaned);
            }
        }

        // Compute outside the lock; the function is pure, so a concurrent
        // duplicate compute is wasted work, never wrong results.
        let cleaned = SchemaCleanr::clean_shared(schema, strategy);
        if Arc::ptr_eq(&cleaned, schema) {
            // No-op clean: nothing worth caching (see the struct docs — a
            // cached no-op would self-pin its source and pollute the map
            // with ephemeral per-call allocations).
            return cleaned;
        }

        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if entries.len() >= SCHEMA_CLEAN_CACHE_CAP && !entries.contains_key(&key) {
            entries.retain(|_, entry| entry.source.strong_count() > 0);
            if entries.len() >= SCHEMA_CLEAN_CACHE_CAP {
                entries.clear();
            }
        }
        entries.insert(
            key,
            SchemaCleanCacheEntry {
                source: Arc::downgrade(schema),
                cleaned: Arc::clone(&cleaned),
            },
        );
        cleaned
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `!needs_cleaning(s)` must imply `clean(s) == s` — the safety contract
    /// that lets `clean_shared` skip the deep copy.
    #[test]
    fn test_needs_cleaning_false_implies_clean_is_identity() {
        let clean_schemas = [
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path" },
                    "recursive": { "type": "boolean", "default": false },
                    "depth": { "type": "integer" }
                },
                "required": ["path"]
            }),
            json!({
                "type": "object",
                "properties": {
                    "items": { "type": "array", "items": { "type": "string" } },
                    "mode": { "type": "string", "enum": ["fast", "slow"] }
                }
            }),
            json!({ "type": "object", "properties": {} }),
        ];
        for strategy in [
            CleaningStrategy::Gemini,
            CleaningStrategy::Anthropic,
            CleaningStrategy::OpenAI,
            CleaningStrategy::Conservative,
        ] {
            for schema in &clean_schemas {
                assert!(
                    !SchemaCleanr::needs_cleaning(schema, strategy),
                    "expected no cleaning needed for {schema} under {strategy:?}"
                );
                assert_eq!(
                    SchemaCleanr::clean(schema.clone(), strategy),
                    *schema,
                    "clean must be identity when needs_cleaning is false ({strategy:?})"
                );
            }
        }
    }

    /// Every rewrite path in the cleaner must be flagged by the pre-scan.
    #[test]
    fn test_needs_cleaning_flags_every_rewrite_trigger() {
        let dirty = [
            // $ref resolution happens for every strategy.
            json!({ "$ref": "#/$defs/Age", "$defs": { "Age": { "type": "integer" } } }),
            // const → enum conversion.
            json!({ "const": "fixed" }),
            // anyOf/oneOf simplification and sibling-type skipping.
            json!({ "anyOf": [{ "type": "string" }, { "type": "null" }] }),
            json!({ "oneOf": [{ "type": "string" }, { "type": "number" }] }),
            // type-array null stripping.
            json!({ "type": ["string", "null"] }),
            // Nested trigger below the top level.
            json!({
                "type": "object",
                "properties": { "role": { "const": "admin" } }
            }),
        ];
        for schema in &dirty {
            assert!(
                SchemaCleanr::needs_cleaning(schema, CleaningStrategy::OpenAI),
                "expected cleaning flagged even for the most permissive strategy: {schema}"
            );
        }
        // Strategy-specific keyword removal.
        let has_min_length = json!({ "type": "string", "minLength": 1 });
        assert!(SchemaCleanr::needs_cleaning(
            &has_min_length,
            CleaningStrategy::Gemini
        ));
        assert!(!SchemaCleanr::needs_cleaning(
            &has_min_length,
            CleaningStrategy::Anthropic
        ));
    }

    #[test]
    fn schema_clean_cache_memoizes_dirty_schema_per_identity() {
        let cache = SchemaCleanCache::new();
        let dirty = Arc::new(json!({ "type": "string", "const": "x" }));

        let first = cache.clean_shared(&dirty, CleaningStrategy::Anthropic);
        let second = cache.clean_shared(&dirty, CleaningStrategy::Anthropic);

        assert!(
            !Arc::ptr_eq(&dirty, &first),
            "dirty schema must be rewritten"
        );
        assert!(
            Arc::ptr_eq(&first, &second),
            "repeated cleaning of the same live schema must return the memoized allocation"
        );
        assert_eq!(
            *first,
            SchemaCleanr::clean((*dirty).clone(), CleaningStrategy::Anthropic),
            "memoized result must equal the uncached cleaner output"
        );
    }

    #[test]
    fn schema_clean_cache_shares_clean_schema_without_inserting() {
        let cache = SchemaCleanCache::new();
        let clean = Arc::new(json!({
            "type": "object",
            "properties": { "path": { "type": "string" } }
        }));

        let shared = cache.clean_shared(&clean, CleaningStrategy::OpenAI);
        assert!(
            Arc::ptr_eq(&clean, &shared),
            "no-op cleaning must share the source Arc, not copy it"
        );
        assert_eq!(
            cache.len(),
            0,
            "no-op results must not be cached: a cached no-op self-pins its \
             source (cleaned aliases it, so the dead-entry prune can never \
             fire) and ephemeral per-call Arcs from the default Tool::spec() \
             would flood the map"
        );
    }

    #[test]
    fn schema_clean_cache_keys_strategies_independently() {
        let cache = SchemaCleanCache::new();
        // Dirty for Gemini (minLength is stripped), no-op for Anthropic.
        let schema = Arc::new(json!({ "type": "string", "minLength": 1 }));

        let gemini = cache.clean_shared(&schema, CleaningStrategy::Gemini);
        let anthropic = cache.clean_shared(&schema, CleaningStrategy::Anthropic);

        assert!(!Arc::ptr_eq(&schema, &gemini));
        assert!(gemini.get("minLength").is_none());
        assert!(
            Arc::ptr_eq(&schema, &anthropic),
            "a strategy the schema is already clean for must still share"
        );
        assert!(
            Arc::ptr_eq(
                &gemini,
                &cache.clean_shared(&schema, CleaningStrategy::Gemini)
            ),
            "each strategy keeps its own memoized entry"
        );
    }

    #[test]
    fn schema_clean_cache_never_serves_stale_result_for_new_schema() {
        let cache = SchemaCleanCache::new();
        let original = Arc::new(json!({ "type": "string", "const": "old" }));
        let original_cleaned = cache.clean_shared(&original, CleaningStrategy::OpenAI);
        assert_eq!(original_cleaned["enum"], json!(["old"]));
        drop(original);

        // A replacement schema (e.g. MCP reconnect) cannot land at the old
        // address while the entry lives — the entry's own `Weak` keeps the
        // old `ArcInner` allocation reserved — so this exercises the plain
        // miss-then-recompute path. Address reuse only becomes possible
        // after the entry (and its `Weak`) is pruned, at which point no
        // stale entry exists to hit. Either way: fresh compute.
        let replacement = Arc::new(json!({ "type": "string", "const": "new" }));
        let replacement_cleaned = cache.clean_shared(&replacement, CleaningStrategy::OpenAI);
        assert_eq!(
            replacement_cleaned["enum"],
            json!(["new"]),
            "cache must never serve a dropped schema's cleaned result"
        );
    }

    #[test]
    fn schema_clean_cache_stays_bounded_when_all_sources_live() {
        let cache = SchemaCleanCache::new();
        // Keep every source alive so the dead-entry prune removes nothing
        // and the overflow path has to fall back to a full clear.
        let sources: Vec<Arc<Value>> = (0..=SCHEMA_CLEAN_CACHE_CAP)
            .map(|i| Arc::new(json!({ "type": "string", "const": format!("v{i}") })))
            .collect();
        for source in &sources {
            cache.clean_shared(source, CleaningStrategy::OpenAI);
        }
        assert!(
            cache.len() <= SCHEMA_CLEAN_CACHE_CAP,
            "cache must never retain more than its cap ({}), got {}",
            SCHEMA_CLEAN_CACHE_CAP,
            cache.len()
        );
    }

    #[test]
    fn schema_clean_cache_overflow_prunes_dead_entries_and_keeps_live_memos() {
        let cache = SchemaCleanCache::new();

        // A long-lived dirty schema (the MCP case this cache exists for).
        let survivor = Arc::new(json!({ "type": "string", "const": "survivor" }));
        let survivor_memo = cache.clean_shared(&survivor, CleaningStrategy::OpenAI);

        // Fill the map exactly to its cap with dirty entries, keeping the
        // sources alive so no mid-fill overflow fires, then drop them all
        // (e.g. per-iteration rebuilt specs going out of scope).
        let ephemerals: Vec<Arc<Value>> = (0..SCHEMA_CLEAN_CACHE_CAP - 1)
            .map(|i| Arc::new(json!({ "type": "string", "const": format!("e{i}") })))
            .collect();
        for ephemeral in &ephemerals {
            cache.clean_shared(ephemeral, CleaningStrategy::OpenAI);
        }
        assert_eq!(cache.len(), SCHEMA_CLEAN_CACHE_CAP);
        drop(ephemerals);

        // The next new-key insert overflows: the graceful tier must drop the
        // dead entries instead of clearing, and the live memo must survive.
        let trigger = Arc::new(json!({ "type": "string", "const": "trigger" }));
        cache.clean_shared(&trigger, CleaningStrategy::OpenAI);
        assert!(
            cache.len() <= 2,
            "overflow with dead entries must prune them (survivor + trigger \
             remain), not fall through to a full clear; got {}",
            cache.len()
        );
        assert!(
            Arc::ptr_eq(
                &survivor_memo,
                &cache.clean_shared(&survivor, CleaningStrategy::OpenAI)
            ),
            "a live schema's memo must survive the dead-entry prune"
        );
    }

    #[test]
    fn test_clean_shared_returns_same_arc_when_clean() {
        let schema = Arc::new(json!({
            "type": "object",
            "properties": { "path": { "type": "string" } }
        }));
        let shared = SchemaCleanr::clean_shared(&schema, CleaningStrategy::Anthropic);
        assert!(
            Arc::ptr_eq(&schema, &shared),
            "clean schema must be shared, not copied"
        );

        let dirty = Arc::new(json!({ "type": "string", "const": "x" }));
        let cleaned = SchemaCleanr::clean_shared(&dirty, CleaningStrategy::Anthropic);
        assert!(!Arc::ptr_eq(&dirty, &cleaned));
        assert_eq!(cleaned["enum"], json!(["x"]));
    }

    #[test]
    fn test_remove_unsupported_keywords() {
        let schema = json!({
            "type": "string",
            "minLength": 1,
            "maxLength": 100,
            "pattern": "^[a-z]+$",
            "description": "A lowercase string"
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        assert_eq!(cleaned["type"], "string");
        assert_eq!(cleaned["description"], "A lowercase string");
        assert!(cleaned.get("minLength").is_none());
        assert!(cleaned.get("maxLength").is_none());
        assert!(cleaned.get("pattern").is_none());
    }

    #[test]
    fn test_resolve_ref() {
        let schema = json!({
            "type": "object",
            "properties": {
                "age": {
                    "$ref": "#/$defs/Age"
                }
            },
            "$defs": {
                "Age": {
                    "type": "integer",
                    "minimum": 0
                }
            }
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        assert_eq!(cleaned["properties"]["age"]["type"], "integer");
        assert!(cleaned["properties"]["age"].get("minimum").is_none()); // Stripped by Gemini strategy
        assert!(cleaned.get("$defs").is_none());
    }

    #[test]
    fn test_resolve_ref_decodes_json_pointer_escapes() {
        let schema = json!({
            "type": "object",
            "properties": {
                "slash": { "$ref": "#/$defs/Foo~1Bar" },
                "tilde": { "$ref": "#/$defs/Tilde~0Name" }
            },
            "$defs": {
                "Foo/Bar": { "type": "string" },
                "Tilde~Name": { "type": "integer" }
            }
        });

        let cleaned = SchemaCleanr::clean_for_anthropic(schema);

        assert_eq!(cleaned["properties"]["slash"]["type"], "string");
        assert_eq!(cleaned["properties"]["tilde"]["type"], "integer");
        assert!(cleaned.get("$defs").is_none());
    }

    #[test]
    fn test_flatten_literal_union() {
        let schema = json!({
            "anyOf": [
                { "const": "admin", "type": "string" },
                { "const": "user", "type": "string" },
                { "const": "guest", "type": "string" }
            ]
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        assert_eq!(cleaned["type"], "string");
        assert!(cleaned["enum"].is_array());
        let enum_values = cleaned["enum"].as_array().unwrap();
        assert_eq!(enum_values.len(), 3);
        assert!(enum_values.contains(&json!("admin")));
        assert!(enum_values.contains(&json!("user")));
        assert!(enum_values.contains(&json!("guest")));
    }

    #[test]
    fn test_strip_null_from_union() {
        let schema = json!({
            "oneOf": [
                { "type": "string" },
                { "type": "null" }
            ]
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        // Should simplify to just { type: "string" }
        assert_eq!(cleaned["type"], "string");
        assert!(cleaned.get("oneOf").is_none());
    }

    #[test]
    fn test_const_to_enum() {
        let schema = json!({
            "const": "fixed_value",
            "description": "A constant"
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        assert_eq!(cleaned["enum"], json!(["fixed_value"]));
        assert_eq!(cleaned["description"], "A constant");
        assert!(cleaned.get("const").is_none());
    }

    #[test]
    fn test_preserve_metadata() {
        let schema = json!({
            "$ref": "#/$defs/Name",
            "description": "User's name",
            "title": "Name Field",
            "default": "Anonymous",
            "$defs": {
                "Name": {
                    "type": "string"
                }
            }
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        assert_eq!(cleaned["type"], "string");
        assert_eq!(cleaned["description"], "User's name");
        assert_eq!(cleaned["title"], "Name Field");
        assert_eq!(cleaned["default"], "Anonymous");
    }

    #[test]
    fn test_circular_ref_prevention() {
        let schema = json!({
            "type": "object",
            "properties": {
                "parent": {
                    "$ref": "#/$defs/Node"
                }
            },
            "$defs": {
                "Node": {
                    "type": "object",
                    "properties": {
                        "child": {
                            "$ref": "#/$defs/Node"
                        }
                    }
                }
            }
        });

        // Should not panic on circular reference
        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        assert_eq!(cleaned["properties"]["parent"]["type"], "object");
        // Circular reference should be broken
    }

    #[test]
    fn test_validate_schema() {
        let valid = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" }
            }
        });

        assert!(SchemaCleanr::validate(&valid).is_ok());

        let invalid = json!({
            "properties": {
                "name": { "type": "string" }
            }
        });

        assert!(SchemaCleanr::validate(&invalid).is_err());
    }

    #[test]
    fn test_strategy_differences() {
        let schema = json!({
            "type": "string",
            "minLength": 1,
            "description": "A string field"
        });

        // Gemini: Most restrictive (removes minLength)
        let gemini = SchemaCleanr::clean_for_gemini(schema.clone());
        assert!(gemini.get("minLength").is_none());
        assert_eq!(gemini["type"], "string");
        assert_eq!(gemini["description"], "A string field");

        // OpenAI: Most permissive (keeps minLength)
        let openai = SchemaCleanr::clean_for_openai(schema.clone());
        assert_eq!(openai["minLength"], 1); // OpenAI allows validation keywords
        assert_eq!(openai["type"], "string");
    }

    #[test]
    fn test_nested_properties() {
        let schema = json!({
            "type": "object",
            "properties": {
                "user": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "minLength": 1
                        }
                    },
                    "additionalProperties": false
                }
            }
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        assert!(
            cleaned["properties"]["user"]["properties"]["name"]
                .get("minLength")
                .is_none()
        );
        assert!(
            cleaned["properties"]["user"]
                .get("additionalProperties")
                .is_none()
        );
    }

    #[test]
    fn test_type_array_null_removal() {
        let schema = json!({
            "type": ["string", "null"]
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        // Should simplify to just "string"
        assert_eq!(cleaned["type"], "string");
    }

    #[test]
    fn test_type_array_only_null_preserved() {
        let schema = json!({
            "type": ["null"]
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        assert_eq!(cleaned["type"], "null");
    }

    #[test]
    fn test_ref_with_json_pointer_escape() {
        let schema = json!({
            "$ref": "#/$defs/Foo~1Bar",
            "$defs": {
                "Foo/Bar": {
                    "type": "string"
                }
            }
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        assert_eq!(cleaned["type"], "string");
    }

    #[test]
    fn test_skip_type_when_non_simplifiable_union_exists() {
        let schema = json!({
            "type": "object",
            "oneOf": [
                {
                    "type": "object",
                    "properties": {
                        "a": { "type": "string" }
                    }
                },
                {
                    "type": "object",
                    "properties": {
                        "b": { "type": "number" }
                    }
                }
            ]
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        assert!(cleaned.get("type").is_none());
        assert!(cleaned.get("oneOf").is_some());
    }

    #[test]
    fn test_clean_nested_unknown_schema_keyword() {
        let schema = json!({
            "not": {
                "$ref": "#/$defs/Age"
            },
            "$defs": {
                "Age": {
                    "type": "integer",
                    "minimum": 0
                }
            }
        });

        let cleaned = SchemaCleanr::clean_for_gemini(schema);

        assert_eq!(cleaned["not"]["type"], "integer");
        assert!(cleaned["not"].get("minimum").is_none());
    }
}
