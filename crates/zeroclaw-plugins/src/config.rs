//! Schema-driven plugin config resolution.
//!
//! The operator's canonical values remain a secret-marked string map in
//! ZeroClaw config, encrypted when persisted. The manifest-owned schema supplies
//! their types. This module materializes a validated JSON object only for the
//! duration of a host call.

#[cfg(any(feature = "plugins-wasmtime", test))]
use std::collections::HashMap;
#[cfg(any(feature = "plugins-wasmtime", test))]
use std::sync::Arc;

#[cfg(any(feature = "plugins-wasmtime", test))]
use serde_json::Map;
use serde_json::Value;

use crate::error::PluginError;
#[cfg(any(feature = "plugins-wasmtime", test))]
use crate::instance::PluginInstanceScope;
use crate::{PluginCapability, PluginManifest, PluginPermission};

const MAX_SCHEMA_BYTES: usize = 64 * 1024;
const MAX_SCHEMA_DEPTH: usize = 32;
const DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

/// Ceiling on one compiled `pattern` program, in bytes.
///
/// A bounded repetition expands into program proportional to its bound, and
/// that expansion is paid at *compile* time. Since `resolve_plugin_config_from`
/// recompiles the schema on every resolution, an expensive pattern is not a
/// one-off admission cost, it is a per-call cost on the host thread. The
/// engine's own default ceiling is 10 MiB, which is high enough that a
/// max-size schema packed with `^[\s\S]{0,8000}$` properties takes tens of
/// seconds to compile - each and every time it resolves.
///
/// 256 KiB was chosen against measurement: it admits the structural patterns
/// operator config actually uses (slugs, UUIDs, emails, URLs, bounded
/// free text up to a few hundred characters) while refusing the
/// large-repetition forms in about a millisecond. Length bounds belong in
/// `maxLength`, which costs nothing to check, not in a repetition count.
const MAX_PATTERN_SIZE_BYTES: usize = 256 * 1024;

/// Ceiling on the lazy-DFA transition cache one compiled pattern may hold.
///
/// Unlike [`MAX_PATTERN_SIZE_BYTES`] this is a cache capacity, not a
/// correctness bound - exceeding it costs cache churn, never a wrong answer -
/// so it only caps steady-state memory per admitted plugin.
const MAX_PATTERN_DFA_BYTES: usize = 1024 * 1024;

/// Root keywords that admit properties by pattern or constraint instead of by
/// name.
///
/// The dialect is deliberately *enumerable*: `resolve_plugin_config_from`
/// materializes an operator value only when the root `properties` map names its
/// key. A schema whose keys are admitted solely by one of these keywords
/// therefore passes admission and validation while every one of those keys is
/// unreachable - the plugin author sees an accepted contract the host never
/// honours. Reject at admission and name the keyword instead.
///
/// `additionalProperties` is absent by design: the root check below already
/// pins it to exactly `false`, which rejects the schema-object form too.
const DYNAMIC_KEY_KEYWORDS: [&str; 3] = [
    "patternProperties",
    "propertyNames",
    "unevaluatedProperties",
];

/// A typed, validated per-use view of one plugin's canonical config section.
///
/// Construction is private so adapters cannot accidentally pass raw or
/// unvalidated operator values to a guest.
#[cfg(any(feature = "plugins-wasmtime", test))]
pub struct ResolvedPluginConfig {
    scope: PluginInstanceScope,
    public_json: Value,
    secrets: HashMap<String, String>,
}

#[cfg(any(feature = "plugins-wasmtime", test))]
impl ResolvedPluginConfig {
    fn new(
        scope: &PluginInstanceScope,
        public_json: Value,
        secrets: HashMap<String, String>,
    ) -> Self {
        Self {
            scope: scope.clone(),
            public_json,
            secrets,
        }
    }

    /// Borrow the validated non-secret JSON object for immediate guest injection.
    #[must_use]
    pub(crate) fn public_json(&self) -> &Value {
        &self.public_json
    }

    /// Borrow one schema-designated secret for an immediate host-mediated use.
    #[must_use]
    pub(crate) fn secret(&self, name: &str) -> Option<&str> {
        self.secrets.get(name).map(String::as_str)
    }

    /// Reject pairing this materialized view with another admission decision.
    pub(crate) fn ensure_scope(&self, scope: &PluginInstanceScope) -> Result<(), PluginError> {
        if !self.scope.same_issuance(scope) {
            return Err(PluginError::InvalidConfig(
                "resolved config belongs to another plugin scope".to_string(),
            ));
        }
        Ok(())
    }
}

/// Host-injected config service. The closure resolves from canonical state on
/// every call; the service carries only that source handle, not a materialized
/// resolved view.
#[derive(Clone)]
#[cfg(any(feature = "plugins-wasmtime", test))]
pub struct PluginConfigResolver {
    resolve: Arc<ResolveConfig>,
}

#[cfg(any(feature = "plugins-wasmtime", test))]
type ResolveConfig = dyn Fn(&PluginInstanceScope) -> Result<ResolvedPluginConfig, PluginError>
    + Send
    + Sync
    + 'static;

#[cfg(any(feature = "plugins-wasmtime", test))]
impl PluginConfigResolver {
    /// Build a resolver whose canonical source may change between service frames.
    #[must_use]
    pub fn new(
        resolve: impl Fn(&PluginInstanceScope) -> Result<ResolvedPluginConfig, PluginError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            resolve: Arc::new(resolve),
        }
    }

    pub fn resolve(
        &self,
        scope: &PluginInstanceScope,
    ) -> Result<ResolvedPluginConfig, PluginError> {
        let config = (self.resolve)(scope)?;
        config.ensure_scope(scope)?;
        Ok(config)
    }
}

/// Validate the manifest-owned config contract without reading operator data.
/// Called during package admission and again while resolving, deliberately
/// avoiding a second stored/compiled copy of the schema.
pub fn validate_manifest_config(manifest: &PluginManifest) -> Result<(), PluginError> {
    compile_manifest_config(manifest).map(drop)
}

/// The single set of compiler options every manifest schema is built with.
///
/// A `config_schema` is untrusted input: it ships inside the plugin package,
/// the default install mode does not require a signature, and
/// `resolve_plugin_config_from` recompiles and revalidates it on *every*
/// resolution. All of that regex work runs on the host thread, outside the
/// guest's fuel budget, so whatever a `pattern` costs, the host pays it
/// repeatedly.
///
/// Two bounds apply, and they cover different halves of the cost:
///
/// - [`MAX_PATTERN_SIZE_BYTES`] bounds *compile* time, which measurement shows
///   is the dominant term. It is the reason a schema cannot make resolution
///   take tens of seconds.
/// - The `regex` engine bounds *match* time. It is linear in the length of the
///   value for every pattern it accepts, so no operator value can trigger
///   super-linear matching. The default `fancy-regex` engine caps backtracking
///   rather than forbidding it, which leaves a real but much smaller
///   per-match amplification.
///
/// The narrower dialect is the deliberate trade. `regex` rejects
/// backreferences and look-around, which `fancy-regex` accepts, so a manifest
/// using them now fails to compile. That is fail-closed: the rejection travels
/// the existing `InvalidManifest` path in `compile_manifest_config`, so
/// admission refuses the package rather than admitting a validator whose cost
/// the host cannot predict. Both limits are documented alongside the other
/// schema limits in `docs/book/src/plugins/migrating-to-typed-config.md`.
///
/// Every validator built from a manifest schema must come from here; building
/// one with `jsonschema::draft202012::new` would restore the default engine
/// and the default 10 MiB program ceiling.
fn manifest_schema_options<'a>() -> jsonschema::ValidationOptions<'a> {
    jsonschema::draft202012::options().with_pattern_options(
        jsonschema::PatternOptions::regex()
            .size_limit(MAX_PATTERN_SIZE_BYTES)
            .dfa_size_limit(MAX_PATTERN_DFA_BYTES),
    )
}

fn compile_manifest_config(
    manifest: &PluginManifest,
) -> Result<Option<jsonschema::Validator>, PluginError> {
    let requests_config = manifest.permissions.contains(&PluginPermission::ConfigRead);
    let Some(schema) = manifest.config_schema.as_ref() else {
        if requests_config {
            return Err(invalid_manifest(format!(
                "plugin '{}' requests config_read but declares no config_schema",
                manifest.name
            )));
        }
        return Ok(None);
    };
    if !requests_config {
        return Err(invalid_manifest(format!(
            "plugin '{}' declares config_schema without requesting config_read",
            manifest.name
        )));
    }

    let schema_bytes = serde_json::to_vec(schema).map_err(|error| {
        invalid_manifest(format!(
            "plugin '{}' config_schema cannot be serialized: {error}",
            manifest.name
        ))
    })?;
    if schema_bytes.len() > MAX_SCHEMA_BYTES {
        return Err(invalid_manifest(format!(
            "plugin '{}' config_schema exceeds {MAX_SCHEMA_BYTES} bytes",
            manifest.name
        )));
    }
    validate_schema_node(schema, 0).map_err(|message| {
        invalid_manifest(format!(
            "plugin '{}' config_schema {message}",
            manifest.name
        ))
    })?;
    validate_secret_annotations(schema).map_err(|message| {
        invalid_manifest(format!(
            "plugin '{}' config_schema {message}",
            manifest.name
        ))
    })?;

    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err(invalid_manifest(format!(
            "plugin '{}' config_schema root type must be object",
            manifest.name
        )));
    }
    if schema.get("additionalProperties") != Some(&Value::Bool(false)) {
        return Err(invalid_manifest(format!(
            "plugin '{}' config_schema must set additionalProperties = false",
            manifest.name
        )));
    }
    if let Some(keyword) = DYNAMIC_KEY_KEYWORDS
        .into_iter()
        .find(|keyword| schema.get(*keyword).is_some())
    {
        return Err(invalid_manifest(format!(
            "plugin '{}' config_schema must not declare '{keyword}' at the root; config values \
             are materialized only for keys named in the properties map, so keys admitted by \
             '{keyword}' would never reach the plugin",
            manifest.name
        )));
    }
    if schema
        .get("$schema")
        .and_then(Value::as_str)
        .is_some_and(|draft| draft != DRAFT_2020_12)
    {
        return Err(invalid_manifest(format!(
            "plugin '{}' config_schema must use JSON Schema Draft 2020-12",
            manifest.name
        )));
    }

    let validator = manifest_schema_options().build(schema).map_err(|error| {
        invalid_manifest(format!(
            "plugin '{}' config_schema cannot be compiled: {}",
            manifest.name,
            error.masked()
        ))
    })?;

    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            invalid_manifest(format!(
                "plugin '{}' config_schema must declare an object properties map",
                manifest.name
            ))
        })?;
    for (name, property) in properties {
        let kind = property_type(schema, property).map_err(|message| {
            invalid_manifest(format!(
                "plugin '{}' config_schema property '{name}' {message}",
                manifest.name
            ))
        })?;
        if is_secret_property(property) && !matches!(kind, PropertyKind::String) {
            return Err(invalid_manifest(format!(
                "plugin '{}' config_schema property '{name}' marks x-secret but does not resolve to type string",
                manifest.name
            )));
        }
    }
    let has_secret_consumer = manifest.capabilities.iter().any(|capability| {
        matches!(
            capability,
            PluginCapability::Tool | PluginCapability::Channel
        )
    });
    if properties.values().any(is_secret_property) && !has_secret_consumer {
        return Err(invalid_manifest(format!(
            "plugin '{}' config_schema uses x-secret without a tool or channel config consumer",
            manifest.name
        )));
    }
    Ok(Some(validator))
}

/// Resolve one admitted instance's effective config into typed JSON and apply
/// every manifest constraint before any guest code receives it.
#[cfg(any(feature = "plugins-wasmtime", test))]
pub fn resolve_plugin_config(
    manifest: &PluginManifest,
    scope: &PluginInstanceScope,
    configured: Option<&HashMap<String, String>>,
) -> Result<ResolvedPluginConfig, PluginError> {
    resolve_plugin_config_from(manifest, scope, || Ok(configured.cloned()))
}

/// Resolve config from a lazily accessed canonical source.
///
/// The source is not called when `config_read` was withheld, so inaccessible
/// secret values are neither cloned nor parsed. Live-config integrations can
/// also acquire their read lock only when values are actually needed.
#[cfg(any(feature = "plugins-wasmtime", test))]
pub fn resolve_plugin_config_from(
    manifest: &PluginManifest,
    scope: &PluginInstanceScope,
    configured: impl FnOnce() -> Result<Option<HashMap<String, String>>, PluginError>,
) -> Result<ResolvedPluginConfig, PluginError> {
    if scope.id().package() != manifest.name {
        return Err(PluginError::InvalidConfig(format!(
            "instance package '{}' does not match manifest '{}'",
            scope.id().package(),
            manifest.name
        )));
    }
    let validator = compile_manifest_config(manifest)?;
    let Some(schema) = manifest.config_schema.as_ref() else {
        let configured = configured()?;
        if configured.is_some_and(|configured| !configured.is_empty()) {
            return Err(PluginError::InvalidConfig(format!(
                "plugin '{}' has configured values but declares no config_schema",
                manifest.name
            )));
        }
        return Ok(ResolvedPluginConfig::new(
            scope,
            Value::Object(Map::new()),
            HashMap::new(),
        ));
    };
    let validator = validator.ok_or_else(|| {
        PluginError::InvalidConfig(format!(
            "plugin '{}' config_schema was not compiled",
            manifest.name
        ))
    })?;
    if !scope.grants().allows(PluginPermission::ConfigRead) {
        let withheld = Value::Object(Map::new());
        validate_config_instance(manifest, &validator, &withheld)?;
        return Ok(ResolvedPluginConfig::new(scope, withheld, HashMap::new()));
    }
    let configured = configured()?.unwrap_or_default();
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            PluginError::InvalidConfig(format!(
                "plugin '{}' admitted config_schema has no properties map",
                manifest.name
            ))
        })?;
    let mut resolved = Map::with_capacity(configured.len());
    for (name, raw) in &configured {
        let property = properties.get(name).ok_or_else(|| {
            PluginError::InvalidConfig(format!(
                "plugin '{}' config contains a property absent from config_schema",
                manifest.name
            ))
        })?;
        let kind = property_type(schema, property).map_err(|message| {
            PluginError::InvalidConfig(format!(
                "plugin '{}' config property '{name}' {message}",
                manifest.name
            ))
        })?;
        resolved.insert(name.clone(), materialize(name, raw, kind)?);
    }
    let resolved = Value::Object(resolved);
    validate_config_instance(manifest, &validator, &resolved)?;

    let Value::Object(resolved) = resolved else {
        return Err(PluginError::InvalidConfig(format!(
            "plugin '{}' resolved config is not an object",
            manifest.name
        )));
    };
    let mut public = Map::with_capacity(resolved.len());
    let mut secrets = HashMap::new();
    for (name, value) in resolved {
        let property = properties.get(&name).ok_or_else(|| {
            PluginError::InvalidConfig(format!(
                "plugin '{}' resolved config contains a property absent from config_schema",
                manifest.name
            ))
        })?;
        if is_secret_property(property) {
            let Value::String(secret) = value else {
                return Err(PluginError::InvalidConfig(format!(
                    "plugin '{}' resolved secret config is not a string",
                    manifest.name
                )));
            };
            secrets.insert(name, secret);
        } else {
            public.insert(name, value);
        }
    }
    Ok(ResolvedPluginConfig::new(
        scope,
        Value::Object(public),
        secrets,
    ))
}

#[cfg(any(feature = "plugins-wasmtime", test))]
fn validate_config_instance(
    manifest: &PluginManifest,
    validator: &jsonschema::Validator,
    config: &Value,
) -> Result<(), PluginError> {
    if let Err(error) = validator.validate(config) {
        return Err(PluginError::InvalidConfig(format!(
            "plugin '{}' config violates config_schema at '{}'",
            manifest.name,
            error.schema_path()
        )));
    }
    Ok(())
}

fn invalid_manifest(message: String) -> PluginError {
    PluginError::InvalidManifest(message)
}

fn validate_schema_node(node: &Value, depth: usize) -> Result<(), String> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(format!("exceeds maximum nesting depth {MAX_SCHEMA_DEPTH}"));
    }
    match node {
        Value::Array(values) => {
            for value in values {
                validate_schema_node(value, depth + 1)?;
            }
        }
        Value::Object(values) => {
            if values.contains_key("$id") {
                return Err("must not declare $id; schemas are package-local".to_string());
            }
            for reference_key in ["$ref", "$dynamicRef", "$recursiveRef"] {
                if let Some(reference) = values.get(reference_key).and_then(Value::as_str) {
                    if !reference.starts_with('#') {
                        return Err(format!(
                            "contains non-local {reference_key} reference '{reference}'"
                        ));
                    }
                    if reference != "#" && !reference.starts_with("#/") {
                        return Err(format!(
                            "contains unsupported {reference_key} anchor '{reference}'; use a local JSON Pointer"
                        ));
                    }
                }
            }
            for value in values.values() {
                validate_schema_node(value, depth + 1)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

fn validate_secret_annotations(schema: &Value) -> Result<(), String> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Location {
        Root,
        TopLevelProperty,
        OtherSchema,
    }

    fn visit_schema(node: &Value, location: Location) -> Result<(), String> {
        let Some(object) = node.as_object() else {
            return Ok(());
        };

        if let Some(marker) = object.get("x-secret") {
            if location != Location::TopLevelProperty {
                return Err("declares x-secret outside a top-level property".to_string());
            }
            if marker != &Value::Bool(true) {
                return Err("x-secret marker must be the boolean value true".to_string());
            }
        }

        for keyword in [
            "additionalProperties",
            "contains",
            "contentSchema",
            "else",
            "if",
            "items",
            "not",
            "propertyNames",
            "then",
            "unevaluatedItems",
            "unevaluatedProperties",
        ] {
            if let Some(child) = object.get(keyword) {
                visit_schema(child, Location::OtherSchema)?;
            }
        }
        for keyword in ["allOf", "anyOf", "oneOf", "prefixItems"] {
            if let Some(children) = object.get(keyword).and_then(Value::as_array) {
                for child in children {
                    visit_schema(child, Location::OtherSchema)?;
                }
            }
        }
        for keyword in [
            "$defs",
            "definitions",
            "dependentSchemas",
            "patternProperties",
            "properties",
        ] {
            if let Some(children) = object.get(keyword).and_then(Value::as_object) {
                for child in children.values() {
                    let child_location = if keyword == "properties" && location == Location::Root {
                        Location::TopLevelProperty
                    } else {
                        Location::OtherSchema
                    };
                    visit_schema(child, child_location)?;
                }
            }
        }
        Ok(())
    }

    visit_schema(schema, Location::Root)
}

fn is_secret_property(property: &Value) -> bool {
    property.get("x-secret") == Some(&Value::Bool(true))
}

#[derive(Clone, Copy)]
enum PropertyKind {
    String,
    Boolean,
    Integer,
    Number,
    Array,
    Object,
}

impl TryFrom<&str> for PropertyKind {
    type Error = String;

    fn try_from(kind: &str) -> Result<Self, Self::Error> {
        match kind {
            "string" => Ok(Self::String),
            "boolean" => Ok(Self::Boolean),
            "integer" => Ok(Self::Integer),
            "number" => Ok(Self::Number),
            "array" => Ok(Self::Array),
            "object" => Ok(Self::Object),
            other => Err(format!("uses unsupported type '{other}'")),
        }
    }
}

fn property_type(root: &Value, property: &Value) -> Result<PropertyKind, String> {
    let mut property = property;
    for _ in 0..=MAX_SCHEMA_DEPTH {
        if let Some(kind) = property.get("type").and_then(Value::as_str) {
            return kind.try_into();
        }
        let reference = property
            .get("$ref")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                "must declare one explicit type (directly or through local $ref)".to_string()
            })?;
        let pointer = reference
            .strip_prefix('#')
            .ok_or_else(|| "must use a local $ref".to_string())?;
        property = root
            .pointer(pointer)
            .ok_or_else(|| format!("references missing schema location '{reference}'"))?;
    }
    Err("contains a cyclic or excessively deep $ref chain".to_string())
}

#[cfg(any(feature = "plugins-wasmtime", test))]
fn materialize(name: &str, raw: &str, kind: PropertyKind) -> Result<Value, PluginError> {
    let parse_json = || {
        serde_json::from_str::<Value>(raw).map_err(|_| {
            PluginError::InvalidConfig(format!("config property '{name}' must contain valid JSON"))
        })
    };
    match kind {
        PropertyKind::String => Ok(Value::String(raw.to_string())),
        PropertyKind::Boolean => {
            let value = parse_json()?;
            value.is_boolean().then_some(value).ok_or_else(|| {
                PluginError::InvalidConfig(format!(
                    "config property '{name}' must be a JSON boolean"
                ))
            })
        }
        PropertyKind::Integer => {
            let value = parse_json()?;
            if value
                .as_number()
                .is_some_and(|number| number.is_i64() || number.is_u64())
            {
                Ok(value)
            } else {
                Err(PluginError::InvalidConfig(format!(
                    "config property '{name}' must be a JSON integer"
                )))
            }
        }
        PropertyKind::Number => {
            let value = parse_json()?;
            value.is_number().then_some(value).ok_or_else(|| {
                PluginError::InvalidConfig(format!(
                    "config property '{name}' must be a JSON number"
                ))
            })
        }
        PropertyKind::Array => {
            let value = parse_json()?;
            value.is_array().then_some(value).ok_or_else(|| {
                PluginError::InvalidConfig(format!("config property '{name}' must be a JSON array"))
            })
        }
        PropertyKind::Object => {
            let value = parse_json()?;
            value.is_object().then_some(value).ok_or_else(|| {
                PluginError::InvalidConfig(format!(
                    "config property '{name}' must be a JSON object"
                ))
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PluginCapability;
    use serde_json::json;

    fn object_schema(properties: Value) -> Value {
        json!({
            "$schema": DRAFT_2020_12,
            "type": "object",
            "properties": properties,
            "additionalProperties": false
        })
    }

    fn manifest(schema: Option<Value>, requests_config: bool) -> PluginManifest {
        PluginManifest {
            name: "fixture".to_string(),
            version: "0.1.0".to_string(),
            description: None,
            author: None,
            wasm_path: Some("fixture.wasm".to_string()),
            capabilities: vec![PluginCapability::Tool],
            permissions: requests_config
                .then_some(PluginPermission::ConfigRead)
                .into_iter()
                .collect(),
            config_schema: schema,
            signature: None,
            publisher_key: None,
        }
    }

    fn scope(manifest: &PluginManifest, grants_config: bool) -> PluginInstanceScope {
        PluginInstanceScope::from_manifest(
            manifest,
            PluginCapability::Tool,
            "main",
            grants_config.then_some(PluginPermission::ConfigRead),
        )
        .expect("valid test scope")
    }

    fn configured(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect()
    }

    fn error_text(result: Result<ResolvedPluginConfig, PluginError>) -> (String, String) {
        let error = result.err().expect("config must be rejected");
        (error.to_string(), format!("{error:?}"))
    }

    #[test]
    fn manifest_requires_config_permission_and_schema_together() {
        assert!(validate_manifest_config(&manifest(None, false)).is_ok());

        let schema = object_schema(json!({}));
        for invalid in [manifest(None, true), manifest(Some(schema.clone()), false)] {
            assert!(matches!(
                validate_manifest_config(&invalid),
                Err(PluginError::InvalidManifest(_))
            ));
        }
        assert!(validate_manifest_config(&manifest(Some(schema), true)).is_ok());
    }

    /// A schema whose keys exist only under a dynamic-key keyword is an
    /// accepted-contract trap: admission and validation both pass, but
    /// resolution walks the root `properties` map, so none of those keys is
    /// ever materialized for the guest. Admission must refuse it and say which
    /// keyword is at fault.
    #[test]
    fn root_dynamic_key_keywords_are_rejected_at_admission() {
        for (keyword, value) in [
            ("patternProperties", json!({"^x-": {"type": "string"}})),
            ("propertyNames", json!({"pattern": "^x-"})),
            ("unevaluatedProperties", json!(false)),
        ] {
            let mut schema = object_schema(json!({"label": {"type": "string"}}));
            schema[keyword] = value;

            let error = validate_manifest_config(&manifest(Some(schema), true))
                .err()
                .unwrap_or_else(|| {
                    panic!(
                        "a root '{keyword}' schema must be refused at admission; admitting it \
                         hands the author a contract whose keys can never be resolved"
                    )
                });
            assert!(
                matches!(error, PluginError::InvalidManifest(_)),
                "a dynamic-key schema is a manifest defect, not a config defect: {error:?}"
            );
            assert!(
                error.to_string().contains(keyword),
                "the admission error must name the offending keyword so the author can find \
                 it; got {error}"
            );
        }
    }

    /// The refusal is about the manifest alone, so it must not quote the
    /// operator's section - these errors surface in install output and logs.
    #[test]
    fn dynamic_key_rejection_does_not_leak_operator_values() {
        let mut schema = object_schema(json!({"label": {"type": "string"}}));
        schema["patternProperties"] = json!({"^secret-": {"type": "string"}});
        let manifest = manifest(Some(schema), true);
        let scope = scope(&manifest, true);
        let configured = configured(&[("label", "s3cr3t-operator-value")]);

        let (display, debug) =
            error_text(resolve_plugin_config(&manifest, &scope, Some(&configured)));
        for rendered in [&display, &debug] {
            assert!(
                !rendered.contains("s3cr3t-operator-value"),
                "the admission error must not quote operator values; got {rendered}"
            );
        }
        assert!(
            display.contains("patternProperties"),
            "the resolve-time refusal must still name the keyword; got {display}"
        );
    }

    /// The neighbouring keywords keep working: the rejection is scoped to the
    /// root, matching how the size, depth, type, and `additionalProperties`
    /// checks are scoped, and does not reach into nested property schemas.
    #[test]
    fn nested_dynamic_key_keywords_are_left_alone() {
        let schema = object_schema(json!({
            "options": {
                "type": "object",
                "patternProperties": {"^x-": {"type": "string"}}
            }
        }));
        validate_manifest_config(&manifest(Some(schema), true)).expect(
            "the enumerable-key rule constrains the root object; a nested property's own \
             schema keeps the full dialect",
        );
    }

    /// A `pattern` is author-controlled and the value matched against it is
    /// operator-controlled, so a nested-quantifier form is the classic
    /// super-linear-matching shape. Admission and resolution together must
    /// stay far below any interactive budget on a value that does not match.
    ///
    /// This is a floor, not proof of the engine choice: with the current
    /// lockfile `(a+)+$` carries no fancy construct, so `fancy-regex` hands it
    /// to the same linear engine and it is fast either way. The assertion
    /// earns its place by catching a future dependency or option change that
    /// puts a backtracking matcher back on this path.
    #[test]
    fn nested_quantifier_patterns_resolve_in_bounded_time() {
        let schema = object_schema(json!({
            "token": {"type": "string", "pattern": "(a+)+$"}
        }));
        let manifest = manifest(Some(schema), true);
        let scope = scope(&manifest, true);
        // Ends in a non-`a`, so the quantifier can never reach `$` and a
        // backtracking matcher explores every partition of the run.
        let hostile = format!("{}b", "a".repeat(128));
        let configured = configured(&[("token", hostile.as_str())]);

        let started = std::time::Instant::now();
        validate_manifest_config(&manifest).expect("a nested quantifier is a legal pattern");
        let resolved = resolve_plugin_config(&manifest, &scope, Some(&configured));
        let elapsed = started.elapsed();

        assert!(
            resolved.is_err(),
            "the value does not match the pattern, so resolution must refuse it"
        );

        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "admission plus resolution took {elapsed:?}; a linear matcher needs microseconds, \
             so anything near this bound means the host is backtracking on operator input"
        );
    }

    /// Backreferences and look-around are exactly the constructs a linear
    /// matcher cannot support, and exactly the ones that make backtracking
    /// super-linear. Refusing them at admission is the fail-closed half of the
    /// bound: the host would rather not run the plugin than run a matcher
    /// whose cost it cannot predict.
    #[test]
    fn patterns_outside_the_linear_dialect_are_refused_at_admission() {
        for pattern in [r"(a+)+\1$", "(?=(a+)+$)x", r"(\w+)\s\1"] {
            let schema = object_schema(json!({
                "token": {"type": "string", "pattern": pattern}
            }));
            let manifest = manifest(Some(schema), true);
            let error = validate_manifest_config(&manifest)
                .err()
                .unwrap_or_else(|| {
                    panic!(
                        "pattern {pattern:?} needs a backtracking matcher; admitting it would \
                         hand the host an unpredictable per-resolution cost"
                    )
                });
            assert!(
                matches!(error, PluginError::InvalidManifest(_)),
                "an uncompilable pattern is a manifest defect, not a config defect: {error:?}"
            );

            let scope = scope(&manifest, true);
            let configured = configured(&[("token", "aa aa")]);
            assert!(
                matches!(
                    resolve_plugin_config(&manifest, &scope, Some(&configured)),
                    Err(PluginError::InvalidManifest(_))
                ),
                "resolution recompiles the schema, so it must refuse {pattern:?} too"
            );
        }
    }

    /// The program a pattern expands into is paid on every resolution, so a
    /// large repetition bound is a per-call cost rather than a one-off. The
    /// engine's own 10 MiB ceiling admits repetitions that take tens of
    /// seconds to compile; this bound refuses them in about a millisecond.
    #[test]
    fn patterns_that_exceed_the_program_size_limit_are_refused_at_admission() {
        let schema = object_schema(json!({
            "token": {"type": "string", "pattern": r"^[\s\S]{0,8000}$"}
        }));
        let manifest = manifest(Some(schema), true);

        let started = std::time::Instant::now();
        let error = validate_manifest_config(&manifest)
            .expect_err("a repetition this large must not be admitted");
        let elapsed = started.elapsed();

        assert!(
            matches!(error, PluginError::InvalidManifest(_)),
            "an over-large pattern is a manifest defect, not a config defect: {error:?}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "the refusal itself took {elapsed:?}; a limit that is only reached after the \
             expensive compile does not bound anything"
        );
    }

    /// The bound above narrows the dialect, so pin the shapes operator config
    /// actually uses. A limit that also refuses these would have traded a
    /// bounded cost for a broken contract.
    #[test]
    fn structural_patterns_operators_use_are_still_admitted() {
        for (pattern, value) in [
            (r"^[a-z0-9-]{1,64}$", "my-plugin-instance"),
            (
                r"^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$",
                "operator@example.com",
            ),
            (
                r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$",
                "3f2504e0-4f89-11d3-9a0c-0305e82c3301",
            ),
            (
                r"^https://[a-zA-Z0-9.-]+(/[^\s]*)?$",
                "https://example.com/hook",
            ),
            (r"^[\s\S]{0,200}$", "a short free-text note"),
        ] {
            let schema = object_schema(json!({
                "token": {"type": "string", "pattern": pattern}
            }));
            let manifest = manifest(Some(schema), true);
            validate_manifest_config(&manifest).unwrap_or_else(|error| {
                panic!("pattern {pattern:?} is ordinary operator config, but was refused: {error}")
            });

            let scope = scope(&manifest, true);
            let configured = configured(&[("token", value)]);
            resolve_plugin_config(&manifest, &scope, Some(&configured)).unwrap_or_else(|error| {
                panic!("value {value:?} matches {pattern:?}, but resolution refused it: {error}")
            });
        }
    }

    #[test]
    fn manifest_toml_deserializes_a_typed_schema() {
        let manifest: PluginManifest = toml::from_str(
            r#"
name = "fixture"
version = "0.1.0"
capabilities = ["tool"]
permissions = ["config_read"]

[config_schema]
"$schema" = "https://json-schema.org/draft/2020-12/schema"
type = "object"
required = ["retries"]
additionalProperties = false

[config_schema.properties.retries]
type = "integer"
minimum = 1

[config_schema.properties.api_key]
type = "string"
x-secret = true
"#,
        )
        .expect("typed manifest schema must deserialize");

        validate_manifest_config(&manifest).expect("deserialized schema must be admitted");
        assert_eq!(
            manifest.config_schema.as_ref().unwrap()["properties"]["retries"]["minimum"],
            1
        );
        assert_eq!(
            manifest.config_schema.as_ref().unwrap()["properties"]["api_key"]["x-secret"],
            true
        );
    }

    #[test]
    fn secret_annotations_are_allowed_only_on_top_level_string_properties() {
        let valid = object_schema(json!({
            "api_key": {"type": "string", "x-secret": true}
        }));
        validate_manifest_config(&manifest(Some(valid), true))
            .expect("a top-level string property may be secret");

        let property_named_like_annotation = object_schema(json!({
            "x-secret": {"type": "string"}
        }));
        validate_manifest_config(&manifest(Some(property_named_like_annotation), true))
            .expect("a property name is not itself an annotation");

        let data_keys_named_like_annotation = object_schema(json!({
            "metadata": {
                "type": "object",
                "properties": {"x-secret": {"type": "string"}},
                "additionalProperties": false,
                "default": {"x-secret": "ordinary data"}
            }
        }));
        validate_manifest_config(&manifest(Some(data_keys_named_like_annotation), true))
            .expect("property names and annotation payload data are not schema markers");

        let invalid_schemas = [
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
                "x-secret": true
            }),
            object_schema(json!({
                "api_key": {"type": "string", "x-secret": false}
            })),
            object_schema(json!({
                "api_key": {"type": "string", "x-secret": "true"}
            })),
            object_schema(json!({
                "retries": {"type": "integer", "x-secret": true}
            })),
            object_schema(json!({
                "credentials": {
                    "type": "object",
                    "properties": {
                        "api_key": {"type": "string", "x-secret": true}
                    },
                    "additionalProperties": false
                }
            })),
            json!({
                "type": "object",
                "properties": {
                    "api_key": {"$ref": "#/$defs/api_key", "x-secret": true}
                },
                "additionalProperties": false,
                "$defs": {
                    "api_key": {"type": "string", "x-secret": true}
                }
            }),
        ];
        for schema in invalid_schemas {
            assert!(matches!(
                validate_manifest_config(&manifest(Some(schema), true)),
                Err(PluginError::InvalidManifest(_))
            ));
        }
    }

    #[test]
    fn secret_annotations_require_a_tool_or_channel_consumer() {
        let schema = object_schema(json!({
            "api_key": {"type": "string", "x-secret": true}
        }));

        let mut channel = manifest(Some(schema.clone()), true);
        channel.capabilities = vec![PluginCapability::Channel];
        validate_manifest_config(&channel).expect("channel may consume scoped secrets");

        let mut mixed = manifest(Some(schema.clone()), true);
        mixed.capabilities = vec![PluginCapability::Tool, PluginCapability::Channel];
        validate_manifest_config(&mixed).expect("mixed tool/channel manifest is supported");

        for capability in [
            PluginCapability::Memory,
            PluginCapability::Observer,
            PluginCapability::Skill,
        ] {
            let mut unsupported = manifest(Some(schema.clone()), true);
            unsupported.capabilities = vec![capability];
            assert!(matches!(
                validate_manifest_config(&unsupported),
                Err(PluginError::InvalidManifest(_))
            ));
        }
    }

    #[test]
    fn resolution_materializes_every_supported_type() {
        let schema = object_schema(json!({
            "label": {"type": "string"},
            "enabled": {"type": "boolean"},
            "retries": {"type": "integer", "minimum": 1},
            "ratio": {"type": "number"},
            "tags": {"type": "array", "items": {"type": "string"}},
            "options": {
                "type": "object",
                "properties": {"nested": {"type": "boolean"}},
                "required": ["nested"],
                "additionalProperties": false
            }
        }));
        let manifest = manifest(Some(schema), true);
        let scope = scope(&manifest, true);
        let configured = configured(&[
            ("label", "007"),
            ("enabled", "true"),
            ("retries", "5"),
            ("ratio", "1.25"),
            ("tags", r#"["alerts","ops"]"#),
            ("options", r#"{"nested":false}"#),
        ]);

        let resolved = resolve_plugin_config(&manifest, &scope, Some(&configured))
            .expect("well-typed config must resolve");
        assert_eq!(
            resolved.public_json(),
            &json!({
                "label": "007",
                "enabled": true,
                "retries": 5,
                "ratio": 1.25,
                "tags": ["alerts", "ops"],
                "options": {"nested": false}
            })
        );
    }

    #[test]
    fn resolution_partitions_secret_properties_out_of_public_json() {
        let mut schema = object_schema(json!({
            "endpoint": {"type": "string"},
            "api_key": {"type": "string", "x-secret": true, "minLength": 8}
        }));
        schema["required"] = json!(["endpoint", "api_key"]);
        let manifest = manifest(Some(schema), true);
        let scope = scope(&manifest, true);
        let values = configured(&[
            ("endpoint", "https://example.test"),
            ("api_key", "secret-value"),
        ]);

        let resolved = resolve_plugin_config(&manifest, &scope, Some(&values))
            .expect("the complete config must validate before partitioning");
        assert_eq!(
            resolved.public_json(),
            &json!({"endpoint": "https://example.test"})
        );
        assert_eq!(resolved.secret("api_key"), Some("secret-value"));
        assert_eq!(resolved.secret("endpoint"), None);
        assert_eq!(resolved.secret("missing"), None);
        assert!(!resolved.public_json().to_string().contains("secret-value"));
    }

    #[test]
    fn required_secret_properties_are_validated_before_partitioning() {
        let mut schema = object_schema(json!({
            "api_key": {"type": "string", "x-secret": true}
        }));
        schema["required"] = json!(["api_key"]);
        let manifest = manifest(Some(schema), true);
        let scope = scope(&manifest, true);

        assert!(resolve_plugin_config(&manifest, &scope, None).is_err());
        let values = configured(&[("api_key", "present")]);
        let resolved = resolve_plugin_config(&manifest, &scope, Some(&values))
            .expect("a required secret must satisfy the complete schema");
        assert_eq!(resolved.public_json(), &json!({}));
        assert_eq!(resolved.secret("api_key"), Some("present"));
    }

    #[test]
    fn secret_properties_may_resolve_string_types_through_local_refs() {
        let schema = json!({
            "$schema": DRAFT_2020_12,
            "type": "object",
            "properties": {
                "api_key": {"$ref": "#/$defs/secret_value", "x-secret": true}
            },
            "additionalProperties": false,
            "$defs": {
                "secret_value": {"type": "string", "minLength": 4}
            }
        });
        let manifest = manifest(Some(schema), true);
        let scope = scope(&manifest, true);
        let values = configured(&[("api_key", "secret")]);

        let resolved = resolve_plugin_config(&manifest, &scope, Some(&values))
            .expect("a secret string type may be supplied through a local ref");
        assert_eq!(resolved.public_json(), &json!({}));
        assert_eq!(resolved.secret("api_key"), Some("secret"));
    }

    #[test]
    fn resolution_rejects_constraints_unknown_keys_and_malformed_values() {
        let mut schema = object_schema(json!({
            "retries": {"type": "integer", "minimum": 2},
            "enabled": {"type": "boolean"}
        }));
        schema["required"] = json!(["retries"]);
        let manifest = manifest(Some(schema), true);
        let scope = scope(&manifest, true);

        for values in [
            configured(&[]),
            configured(&[("unknown", "value")]),
            configured(&[("retries", "1")]),
            configured(&[("retries", "2"), ("enabled", "TRUE")]),
        ] {
            assert!(resolve_plugin_config(&manifest, &scope, Some(&values)).is_err());
        }
    }

    #[test]
    fn withheld_grant_validates_and_returns_only_an_empty_object() {
        let optional_manifest = manifest(
            Some(object_schema(json!({
                "enabled": {"type": "boolean"},
                "api_key": {"type": "string", "x-secret": true}
            }))),
            true,
        );
        let optional_scope = scope(&optional_manifest, false);
        for inaccessible in [
            configured(&[("enabled", "not-a-boolean")]),
            configured(&[("api_key", "host-only-secret")]),
            configured(&[("unknown-secret-key", "host-only-secret")]),
        ] {
            let resolved =
                resolve_plugin_config(&optional_manifest, &optional_scope, Some(&inaccessible))
                    .expect("withheld values must not be parsed or validated");
            assert_eq!(resolved.public_json(), &json!({}));
        }

        let source_called = std::cell::Cell::new(false);
        resolve_plugin_config_from(&optional_manifest, &optional_scope, || {
            source_called.set(true);
            Ok(Some(configured(&[("enabled", "true")])))
        })
        .expect("withheld config must resolve without consulting its source");
        assert!(!source_called.get());

        let mut required_schema = object_schema(json!({"api_key": {"type": "string"}}));
        required_schema["required"] = json!(["api_key"]);
        let required_manifest = manifest(Some(required_schema), true);
        let required_scope = scope(&required_manifest, false);
        assert!(
            resolve_plugin_config(&required_manifest, &required_scope, None).is_err(),
            "withheld required config must fail closed on the empty object"
        );
    }

    #[test]
    fn resolver_reads_canonical_values_on_every_call() {
        let manifest = Arc::new(manifest(
            Some(object_schema(json!({"enabled": {"type": "boolean"}}))),
            true,
        ));
        let scope = scope(&manifest, true);
        let values = Arc::new(std::sync::RwLock::new(configured(&[("enabled", "false")])));
        let resolver_manifest = Arc::clone(&manifest);
        let resolver_values = Arc::clone(&values);
        let resolver = PluginConfigResolver::new(move |scope| {
            let values = resolver_values.read().unwrap();
            resolve_plugin_config(&resolver_manifest, scope, Some(&values))
        });

        assert_eq!(
            resolver.resolve(&scope).unwrap().public_json()["enabled"],
            false
        );
        values
            .write()
            .unwrap()
            .insert("enabled".to_string(), "true".to_string());
        assert_eq!(
            resolver.resolve(&scope).unwrap().public_json()["enabled"],
            true
        );
    }

    #[test]
    fn resolver_rejects_a_view_from_another_scope_issuance() {
        let manifest = Arc::new(manifest(Some(object_schema(json!({}))), true));
        let requested_scope = scope(&manifest, true);
        let other_scope = scope(&manifest, true);
        assert_eq!(requested_scope.id(), other_scope.id());
        assert!(!requested_scope.same_issuance(&other_scope));

        let resolver_manifest = Arc::clone(&manifest);
        let resolver_scope = other_scope.clone();
        let resolver = PluginConfigResolver::new(move |_| {
            resolve_plugin_config(&resolver_manifest, &resolver_scope, None)
        });

        assert!(matches!(
            resolver.resolve(&requested_scope),
            Err(PluginError::InvalidConfig(_))
        ));
    }

    #[test]
    fn resolution_rejects_a_scope_for_another_package() {
        let manifest = manifest(Some(object_schema(json!({}))), true);
        let scope = scope(&manifest, true);
        let mut other_manifest = manifest.clone();
        other_manifest.name = "other".to_string();

        let (display, _) = error_text(resolve_plugin_config(&other_manifest, &scope, None));
        assert!(display.contains("does not match"));
    }

    #[test]
    fn schema_restrictions_fail_closed_and_local_pointer_refs_work() {
        let invalid_schemas = [
            json!({"type": "string", "properties": {}, "additionalProperties": false}),
            json!({"type": "object", "properties": {}, "additionalProperties": true}),
            object_schema(json!({"value": {"type": "null"}})),
            object_schema(json!({"value": {"$ref": "https://example.test/schema"}})),
            object_schema(json!({"value": {"$ref": "#named-anchor"}})),
            json!({
                "$id": "https://example.test/schema",
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ];
        for schema in invalid_schemas {
            assert!(validate_manifest_config(&manifest(Some(schema), true)).is_err());
        }

        let schema = json!({
            "$schema": DRAFT_2020_12,
            "type": "object",
            "properties": {"retries": {"$ref": "#/$defs/retries"}},
            "additionalProperties": false,
            "$defs": {"retries": {"type": "integer", "minimum": 1}}
        });
        let manifest = manifest(Some(schema), true);
        let scope = scope(&manifest, true);
        let values = configured(&[("retries", "3")]);
        assert!(resolve_plugin_config(&manifest, &scope, Some(&values)).is_ok());
    }

    #[test]
    fn schema_size_and_depth_limits_are_enforced() {
        let mut oversized = object_schema(json!({}));
        oversized["description"] = Value::String("x".repeat(MAX_SCHEMA_BYTES));
        assert!(validate_manifest_config(&manifest(Some(oversized), true)).is_err());

        let mut nested = Value::Bool(true);
        for _ in 0..=MAX_SCHEMA_DEPTH {
            nested = json!({"annotation": nested});
        }
        let mut too_deep = object_schema(json!({}));
        too_deep["annotation"] = nested;
        assert!(validate_manifest_config(&manifest(Some(too_deep), true)).is_err());
    }

    #[test]
    fn config_errors_do_not_expose_secret_values_or_nested_secret_keys() {
        let scalar_secret = "sk-live-value-must-not-leak";
        let scalar_manifest = manifest(
            Some(object_schema(json!({
                "api_key": {"type": "string", "x-secret": true, "minLength": 100}
            }))),
            true,
        );
        let scalar_scope = scope(&scalar_manifest, true);
        let values = configured(&[("api_key", scalar_secret)]);
        let (display, debug) = error_text(resolve_plugin_config(
            &scalar_manifest,
            &scalar_scope,
            Some(&values),
        ));
        assert!(!display.contains(scalar_secret));
        assert!(!debug.contains(scalar_secret));

        let secret_key = "sk-live-key-must-not-leak";
        let nested_secret = "nested-value-must-not-leak";
        let unknown = configured(&[(secret_key, nested_secret)]);
        let (display, debug) = error_text(resolve_plugin_config(
            &scalar_manifest,
            &scalar_scope,
            Some(&unknown),
        ));
        for secret in [secret_key, nested_secret] {
            assert!(!display.contains(secret));
            assert!(!debug.contains(secret));
        }

        let object_manifest = manifest(
            Some(object_schema(json!({
                "credentials": {
                    "type": "object",
                    "properties": {"allowed": {"type": "string"}},
                    "additionalProperties": false
                }
            }))),
            true,
        );
        let object_scope = scope(&object_manifest, true);
        let encoded = format!(r#"{{"{secret_key}":"{nested_secret}"}}"#);
        let values = configured(&[("credentials", &encoded)]);
        let (display, debug) = error_text(resolve_plugin_config(
            &object_manifest,
            &object_scope,
            Some(&values),
        ));
        for secret in [secret_key, nested_secret] {
            assert!(!display.contains(secret));
            assert!(!debug.contains(secret));
        }
    }
}
