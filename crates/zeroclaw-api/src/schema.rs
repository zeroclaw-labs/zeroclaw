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

/// Per-provider bounds for completed cleaned-schema memos. The byte bound is
/// deliberately small because MCP schemas are externally supplied and have no
/// intrinsic size limit; the entry bound prevents many tiny trees from
/// accumulating metadata indefinitely.
const SCHEMA_CLEAN_CACHE_MAX_ENTRIES: usize = 64;
const SCHEMA_CLEAN_CACHE_MAX_BYTES: usize = 4 * 1024 * 1024;

struct SchemaCleanCacheEntry {
    /// Identity of the source schema this result was cleaned from. `Weak`
    /// so a cache entry never keeps a replaced (e.g. MCP-reconnect) schema
    /// alive on its own.
    source: std::sync::Weak<Value>,
    /// Single-flight cell for the deep-clean result. The map lock is only
    /// ever held to install or look up this cell, never while the clean
    /// itself runs: the first caller to reach [`OnceLock::get_or_init`] on a
    /// given cell performs the deep clone, and every other caller that
    /// reused the same entry (matched by `source`) blocks on that same
    /// `get_or_init` call and observes the identical `Arc` once it
    /// resolves. A `(schema, strategy)` key is therefore deep-cleaned at
    /// most once even when several threads race a cold miss together;
    /// unrelated keys use unrelated cells, so they still clean
    /// concurrently.
    cleaned: Arc<std::sync::OnceLock<Arc<Value>>>,
    /// Conservative heap-size estimate for an initialized `cleaned` tree.
    /// Shared with the initializer so it can publish the size without
    /// reacquiring the map lock.
    retained_bytes: Arc<std::sync::atomic::AtomicUsize>,
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
/// Retired sources are pruned whenever a dirty schema uses the cache, so a
/// replaced MCP schema does not wait for capacity pressure before its cleaned
/// tree can be released. Completed entries are additionally bounded per
/// provider by both count and estimated heap bytes. When a new result would
/// exceed either bound, admission is declined for that result instead of
/// evicting the established working set. In-flight single-flight cells are
/// never evicted; concurrent callers participating in one cold miss still
/// share exactly one computation.
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
    /// Counts actual deep-clean computations (`OnceLock::get_or_init`
    /// closure runs), as opposed to cache hits or single-flight waits.
    /// Test-only: lets the concurrent single-flight regression assert that
    /// N racing callers on the same key produced exactly one deep clone,
    /// not merely that they converged on the same returned pointer (a
    /// post-compute recheck could stabilize the pointer while still having
    /// done the duplicate work).
    #[cfg(test)]
    cold_compute_count: std::sync::atomic::AtomicUsize,
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
            #[cfg(test)]
            cold_compute_count: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Memoized [`SchemaCleanr::clean_shared`]: returns the shared source
    /// `Arc` when cleaning is a no-op, and otherwise the cleaned tree —
    /// deep-computed at most once per retained (live schema, strategy) pair,
    /// even when multiple threads race a cold miss on the same key together.
    ///
    /// Single-flight: the first miss for a `(schema, strategy)` key installs
    /// a shared [`OnceLock`](std::sync::OnceLock) cell in the map before the
    /// lock is released. Any other thread that misses on the *same* key
    /// (i.e. it upgrades to the same live `source`) finds that cell already
    /// installed, reuses it, and blocks in `get_or_init` instead of starting
    /// its own deep clone — so only the winner's closure ever runs, and
    /// every caller, winner and waiters alike, ends up with the one
    /// resulting `Arc`. Misses on *different* keys install independent cells
    /// and clean fully concurrently; the map lock is never held while a clean
    /// itself runs. Capacity enforcement can decline admission only for the
    /// just-completed cell, so it cannot split an in-flight computation into
    /// competing cells or discard the established working set.
    pub fn clean_shared(&self, schema: &Arc<Value>, strategy: CleaningStrategy) -> Arc<Value> {
        if !SchemaCleanr::needs_cleaning(schema, strategy) {
            // No-op: nothing worth caching or single-flighting (see the
            // struct docs — a cached no-op would self-pin its source and
            // pollute the map with ephemeral per-call allocations).
            return Arc::clone(schema);
        }

        let key = (Arc::as_ptr(schema) as usize, strategy);
        let (cell, retained_bytes) = {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // An actual in-flight caller still owns the source `Arc`, so a
            // dead source is also proof that its entry is safe to remove.
            entries.retain(|_, entry| entry.source.strong_count() > 0);
            if let Some(entry) = entries.get(&key)
                && let Some(live_source) = entry.source.upgrade()
                && Arc::ptr_eq(&live_source, schema)
            {
                (
                    Arc::clone(&entry.cleaned),
                    Arc::clone(&entry.retained_bytes),
                )
            } else {
                let cell = Arc::new(std::sync::OnceLock::new());
                let retained_bytes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                entries.insert(
                    key,
                    SchemaCleanCacheEntry {
                        source: Arc::downgrade(schema),
                        cleaned: Arc::clone(&cell),
                        retained_bytes: Arc::clone(&retained_bytes),
                    },
                );
                (cell, retained_bytes)
            }
        };

        // Outside the lock: `needs_cleaning` already proved above that this
        // schema requires a real rewrite, so `SchemaCleanr::clean_shared`
        // cannot take its no-op path here — it always performs (or, for
        // waiters, would have performed) the deep clone. Only the caller
        // that actually initializes `cell` runs the closure; concurrent
        // callers sharing `cell` block here and all observe that same
        // result `Arc`.
        let cleaned = Arc::clone(cell.get_or_init(|| {
            #[cfg(test)]
            self.cold_compute_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let cleaned = SchemaCleanr::clean_shared(schema, strategy);
            retained_bytes.store(
                estimated_json_heap_bytes(&cleaned),
                std::sync::atomic::Ordering::Relaxed,
            );
            cleaned
        }));
        self.enforce_retention_bounds(key, &cell);
        cleaned
    }

    fn enforce_retention_bounds(
        &self,
        current_key: (usize, CleaningStrategy),
        current_cell: &Arc<std::sync::OnceLock<Arc<Value>>>,
    ) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.retain(|_, entry| entry.source.strong_count() > 0);

        let completed_count = entries
            .values()
            .filter(|entry| entry.cleaned.get().is_some())
            .count();
        let completed_bytes = entries.values().fold(0usize, |total, entry| {
            total.saturating_add(
                entry
                    .retained_bytes
                    .load(std::sync::atomic::Ordering::Relaxed),
            )
        });
        if completed_count <= SCHEMA_CLEAN_CACHE_MAX_ENTRIES
            && completed_bytes <= SCHEMA_CLEAN_CACHE_MAX_BYTES
        {
            return;
        }

        // Preserve the already-admitted working set. Clearing every completed
        // entry here makes a stable roster just one item over the cap thrash:
        // the last schema evicts all earlier schemas, then the next request
        // deep-cleans almost the entire roster again. Removing only the
        // just-completed cell implements admission control: the established
        // bounded set stays hot, while an over-budget schema is recomputed on
        // its next use. The identity check prevents an old caller from
        // removing a replacement entry installed at the same key.
        if entries.get(&current_key).is_some_and(|entry| {
            entry.cleaned.get().is_some() && Arc::ptr_eq(&entry.cleaned, current_cell)
        }) {
            entries.remove(&current_key);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Test-only: number of times the deep-clean closure actually ran
    /// (as opposed to cache hits or single-flight waits).
    #[cfg(test)]
    fn cold_compute_count(&self) -> usize {
        self.cold_compute_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Estimate the heap retained by a JSON tree without serializing or allocating
/// a second representation. Container overhead is intentionally rounded up;
/// this is a retention guard, not an allocator accounting API.
fn estimated_json_heap_bytes(value: &Value) -> usize {
    fn heap_bytes(value: &Value) -> usize {
        match value {
            Value::Null | Value::Bool(_) | Value::Number(_) => 0,
            Value::String(text) => text.capacity(),
            Value::Array(items) => items.iter().fold(
                items
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Value>()),
                |total, child| total.saturating_add(heap_bytes(child)),
            ),
            Value::Object(entries) => {
                const MAP_NODE_OVERHEAD: usize = 3 * std::mem::size_of::<usize>();
                entries.iter().fold(0usize, |total, (key, child)| {
                    total.saturating_add(
                        std::mem::size_of::<String>()
                            + std::mem::size_of::<Value>()
                            + MAP_NODE_OVERHEAD
                            + key.capacity()
                            + heap_bytes(child),
                    )
                })
            }
        }
    }

    std::mem::size_of::<Value>().saturating_add(heap_bytes(value))
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
    fn schema_clean_cache_stays_entry_bounded_when_all_sources_live() {
        let cache = SchemaCleanCache::new();
        let sources: Vec<Arc<Value>> = (0..=SCHEMA_CLEAN_CACHE_MAX_ENTRIES)
            .map(|i| Arc::new(json!({ "type": "string", "const": format!("v{i}") })))
            .collect();
        for source in &sources {
            cache.clean_shared(source, CleaningStrategy::OpenAI);
        }
        assert!(
            cache.len() <= SCHEMA_CLEAN_CACHE_MAX_ENTRIES,
            "cache must never retain more than its cap ({}), got {}",
            SCHEMA_CLEAN_CACHE_MAX_ENTRIES,
            cache.len()
        );
    }

    #[test]
    fn schema_clean_cache_preserves_working_set_when_roster_exceeds_entry_cap() {
        let cache = SchemaCleanCache::new();
        let sources: Vec<Arc<Value>> = (0..=SCHEMA_CLEAN_CACHE_MAX_ENTRIES)
            .map(|i| Arc::new(json!({ "type": "string", "const": format!("v{i}") })))
            .collect();

        for source in &sources {
            cache.clean_shared(source, CleaningStrategy::OpenAI);
        }
        assert_eq!(
            cache.cold_compute_count(),
            sources.len(),
            "the first pass must clean each distinct dirty schema once"
        );

        for source in &sources {
            cache.clean_shared(source, CleaningStrategy::OpenAI);
        }
        assert_eq!(
            cache.cold_compute_count(),
            sources.len() + 1,
            "a stable roster one schema over the cap should recompute only the \
             unadmitted schema, not evict and rebuild the retained working set"
        );
        assert_eq!(cache.len(), SCHEMA_CLEAN_CACHE_MAX_ENTRIES);
    }

    #[test]
    fn schema_clean_cache_prunes_retired_source_without_capacity_pressure() {
        let cache = SchemaCleanCache::new();
        let retired = Arc::new(json!({ "type": "string", "const": "retired" }));
        let retired_cleaned = cache.clean_shared(&retired, CleaningStrategy::OpenAI);
        let retired_cleaned_weak = Arc::downgrade(&retired_cleaned);
        drop(retired_cleaned);
        drop(retired);
        assert!(
            retired_cleaned_weak.upgrade().is_some(),
            "the cache should own the cleaned allocation before retirement"
        );

        let active = Arc::new(json!({ "type": "string", "const": "active" }));
        cache.clean_shared(&active, CleaningStrategy::OpenAI);

        assert!(
            retired_cleaned_weak.upgrade().is_none(),
            "ordinary dirty-schema activity must release a retired source's \
             cleaned allocation without filling the cache first"
        );
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn schema_clean_cache_does_not_retain_one_oversized_external_schema() {
        let cache = SchemaCleanCache::new();
        let oversized = Arc::new(json!({
            "type": "string",
            "const": "x".repeat(SCHEMA_CLEAN_CACHE_MAX_BYTES)
        }));

        let cleaned = cache.clean_shared(&oversized, CleaningStrategy::OpenAI);
        assert_eq!(&cleaned["enum"][0], oversized.get("const").unwrap());
        assert_eq!(
            cache.len(),
            0,
            "one externally supplied schema larger than the byte budget must \
             not remain retained by the provider cache"
        );
    }

    #[test]
    fn schema_clean_cache_pressure_never_evicts_in_flight_cell() {
        let cache = SchemaCleanCache::new();
        let in_flight_source = Arc::new(json!({ "type": "string", "const": "in-flight" }));
        let in_flight_key = (
            Arc::as_ptr(&in_flight_source) as usize,
            CleaningStrategy::OpenAI,
        );
        let in_flight_cell = Arc::new(std::sync::OnceLock::new());
        cache
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                in_flight_key,
                SchemaCleanCacheEntry {
                    source: Arc::downgrade(&in_flight_source),
                    cleaned: Arc::clone(&in_flight_cell),
                    retained_bytes: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                },
            );

        let live_sources: Vec<Arc<Value>> = (0..=SCHEMA_CLEAN_CACHE_MAX_ENTRIES)
            .map(|i| Arc::new(json!({ "type": "string", "const": format!("v{i}") })))
            .collect();
        for source in &live_sources {
            cache.clean_shared(source, CleaningStrategy::OpenAI);
        }

        let entries = cache
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            entries.get(&in_flight_key).is_some_and(|entry| {
                entry.cleaned.get().is_none() && Arc::ptr_eq(&entry.cleaned, &in_flight_cell)
            }),
            "capacity enforcement must retain the installed single-flight \
             cell until its first computation finishes"
        );
    }

    /// Regression for a single-flight gap: releasing the map lock before
    /// cleaning let two concurrent cold misses on the *same* `(schema,
    /// strategy)` key both deep-clean and then race to insert, so callers
    /// could observe different allocations for what should be one memoized
    /// result. Races many threads on a common start line against one dirty
    /// schema and asserts both that the deep clean ran exactly once (via
    /// the test-only compute counter, not just pointer convergence — a
    /// post-compute recheck could stabilize the returned pointer while
    /// still having done the duplicate work) and that every racer shares
    /// that one allocation.
    #[test]
    fn schema_clean_cache_single_flights_concurrent_cold_miss_on_same_key() {
        const RACERS: usize = 32;

        let cache = Arc::new(SchemaCleanCache::new());
        // A schema with real fan-out so a duplicate deep clone is not just
        // wasted CPU cycles but a genuinely distinct tree.
        let properties: Map<String, Value> = (0..64)
            .map(|i| (format!("field_{i}"), json!({ "const": format!("v{i}") })))
            .collect();
        let dirty: Arc<Value> = Arc::new(json!({
            "type": "object",
            "properties": Value::Object(properties)
        }));

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(RACERS));
        let handles: Vec<_> = (0..RACERS)
            .map(|_| {
                let cache = Arc::clone(&cache);
                let dirty = Arc::clone(&dirty);
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    // Every racer blocks here until all RACERS threads have
                    // started, so the misses genuinely overlap instead of
                    // serializing through thread-spawn latency.
                    barrier.wait();
                    cache.clean_shared(&dirty, CleaningStrategy::Anthropic)
                })
            })
            .collect();

        let results: Vec<Arc<Value>> = handles
            .into_iter()
            .map(|h| h.join().expect("racer thread panicked"))
            .collect();

        assert_eq!(
            cache.cold_compute_count(),
            1,
            "single-flight must deep-clean exactly once for a (schema, \
             strategy) key raced by {RACERS} concurrent callers; a count \
             greater than 1 means concurrent misses each deep-cloned \
             independently instead of sharing the first computation"
        );

        let first = &results[0];
        assert!(
            !Arc::ptr_eq(first, &dirty),
            "dirty schema must actually be rewritten by the single \
             computation, not shared as-is"
        );
        for (i, result) in results.iter().enumerate() {
            assert!(
                Arc::ptr_eq(first, result),
                "racer {i} observed a different allocation than racer 0; \
                 every concurrent caller on the same key must share the \
                 one single-flighted result"
            );
        }
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
