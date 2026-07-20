use serde_json::Value;

pub fn evaluate_condition(condition: &str, payload: Option<&str>) -> bool {
    let condition = condition.trim();
    if condition.is_empty() {
        return true; // empty condition = unconditional match
    }

    let payload = match payload {
        Some(p) if !p.is_empty() => p,
        _ => return false, // no payload to evaluate against
    };

    if let Some(rest) = condition.strip_prefix('$') {
        // JSON path condition: $.key.sub >= 85
        evaluate_json_path_condition(rest, payload)
    } else {
        // Direct comparison: > 0
        evaluate_direct_condition(condition, payload)
    }
}

/// Evaluate `$.path.to.field op value` against a JSON payload.
fn evaluate_json_path_condition(path_and_op: &str, payload: &str) -> bool {
    let json: Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(_) => return false,
    };

    // Split into (dot_path, operator, comparand)
    let (dot_path, op, comparand) = match parse_path_op_value(path_and_op) {
        Some(t) => t,
        None => return false,
    };

    let extracted = resolve_json_path(&json, &dot_path);
    let extracted = match extracted {
        Some(v) => v,
        None => return false,
    };

    compare_values(extracted, op, &comparand)
}

/// Evaluate `op value` directly against the payload (treated as a number).
fn evaluate_direct_condition(condition: &str, payload: &str) -> bool {
    let (op, comparand) = match parse_op_value(condition) {
        Some(t) => t,
        None => return false,
    };

    // Try to parse payload as a number
    let payload_num: f64 = match payload.trim().parse() {
        Ok(n) => n,
        Err(_) => return false,
    };

    let comparand_num: f64 = match comparand.parse() {
        Ok(n) => n,
        Err(_) => return false,
    };

    apply_op_f64(payload_num, op, comparand_num)
}

// ── Parsing helpers ─────────────────────────────────────────────

/// Comparison operators, longest-token-first so parsing never mistakes a
/// two-char token (`>=`) for its one-char prefix (`>`). This order is the
/// single scan order every parser and every authoring surface reads.
fn parse_order() -> [ConditionOp; 6] {
    [
        ConditionOp::Gte,
        ConditionOp::Lte,
        ConditionOp::Neq,
        ConditionOp::Eq,
        ConditionOp::Gt,
        ConditionOp::Lt,
    ]
}

/// Parse `".path.to.field op value"` → `(["path","to","field"], op, "value")`.
fn parse_path_op_value(input: &str) -> Option<(Vec<&str>, ConditionOp, String)> {
    // Input starts after `$`, e.g. `.value > 85` or `.data.temp >= 100`
    for op in parse_order() {
        if let Some(pos) = input.find(op.token()) {
            let path_part = input[..pos].trim();
            let value_part = input[pos + op.token().len()..].trim();

            if value_part.is_empty() {
                return None;
            }

            let segments: Vec<&str> = path_part.split('.').filter(|s| !s.is_empty()).collect();

            if segments.is_empty() {
                return None;
            }

            return Some((segments, op, value_part.to_string()));
        }
    }
    None
}

/// Parse `"op value"` → `(op, "value")`.
fn parse_op_value(input: &str) -> Option<(ConditionOp, String)> {
    let input = input.trim();
    for op in parse_order() {
        if let Some(rest) = input.strip_prefix(op.token()) {
            let value = rest.trim();
            if value.is_empty() {
                return None;
            }
            return Some((op, value.to_string()));
        }
    }
    None
}

/// Walk a JSON value by dot-separated path segments.
fn resolve_json_path<'a>(value: &'a Value, segments: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for &seg in segments {
        // Try object key
        if let Some(next) = current.get(seg) {
            current = next;
            continue;
        }
        // Try array index
        if let Ok(idx) = seg.parse::<usize>()
            && let Some(next) = current.get(idx)
        {
            current = next;
            continue;
        }
        return None;
    }
    Some(current)
}

// ── Comparison ──────────────────────────────────────────────────

/// A condition comparison operator. This enum is the single source of truth for
/// the operator set: the parser scans its tokens, the evaluator matches on its
/// variants, and every authoring surface renders the list this enum yields (via
/// [`ConditionOp::catalog`]). Adding an operator here is the only edit needed;
/// no surface hand-lists operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum_macros::EnumIter)]
pub enum ConditionOp {
    Gt,
    Lt,
    Gte,
    Lte,
    Eq,
    Neq,
}

impl ConditionOp {
    /// The literal token as it appears in a condition string.
    pub fn token(self) -> &'static str {
        match self {
            Self::Gt => ">",
            Self::Lt => "<",
            Self::Gte => ">=",
            Self::Lte => "<=",
            Self::Eq => "==",
            Self::Neq => "!=",
        }
    }

    /// A short human label for pickers ("is", "is greater than", ...).
    pub fn label(self) -> &'static str {
        match self {
            Self::Eq => "is",
            Self::Neq => "is not",
            Self::Gt => "is greater than",
            Self::Lt => "is less than",
            Self::Gte => "is at least",
            Self::Lte => "is at most",
        }
    }

    /// The full operator catalog in canonical display order (equality first,
    /// then ordering), for authoring surfaces to render verbatim.
    pub fn catalog() -> Vec<ConditionOpSpec> {
        use strum::IntoEnumIterator;
        [
            Self::Eq,
            Self::Neq,
            Self::Gt,
            Self::Gte,
            Self::Lt,
            Self::Lte,
        ]
        .into_iter()
        .map(|op| {
            debug_assert!(Self::iter().any(|variant| variant == op));
            ConditionOpSpec {
                token: op.token().to_string(),
                label: op.label().to_string(),
            }
        })
        .collect()
    }

    /// Every operator token, for the parser's longest-first scan. Walks the
    /// same enum the catalog does, so no token literal is hand-listed twice.
    pub fn catalog_tokens() -> Vec<&'static str> {
        use strum::IntoEnumIterator;
        Self::iter().map(Self::token).collect()
    }
}

/// Wire shape of one operator for authoring surfaces: the literal `token` to
/// splice into a condition string, and a human `label` to show in a picker.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ConditionOpSpec {
    pub token: String,
    pub label: String,
}

/// A condition string decomposed into the three parts an authoring surface
/// edits: an optional JSON path (absent for `direct` scalar payloads), the
/// operator token, and the raw comparand. This is the single authority both
/// the web and zerocode builders round-trip through, so the two surfaces
/// assemble identical strings from identical parts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConditionParts {
    pub path: Option<String>,
    pub op: String,
    pub value: String,
}

impl ConditionParts {
    /// Split a stored condition into parts against the operator catalog.
    /// Tokens are matched longest first so `>=` wins over `>`. A `$`-prefixed
    /// input is JSON-path form; anything else is a direct scalar comparison
    /// with no path. Unmatched input yields an empty `op` so the caller can
    /// fall back to raw editing without losing the text.
    #[must_use]
    pub fn parse(condition: &str) -> Self {
        let trimmed = condition.trim();
        let has_path = trimmed.starts_with('$');
        let scan_from = if has_path {
            trimmed
                .trim_start_matches('$')
                .trim_start_matches('.')
                .trim()
        } else {
            trimmed
        };
        let mut tokens: Vec<&'static str> = ConditionOp::catalog_tokens();
        tokens.sort_by_key(|b| std::cmp::Reverse(b.len()));
        for token in tokens {
            if let Some(at) = scan_from.find(token) {
                let left = scan_from[..at].trim().to_string();
                let right = scan_from[at + token.len()..].trim().to_string();
                return Self {
                    path: has_path.then_some(left),
                    op: token.to_string(),
                    value: right,
                };
            }
        }
        Self {
            path: has_path.then(|| scan_from.trim().to_string()),
            op: String::new(),
            value: String::new(),
        }
    }

    /// Reassemble a condition string. JSON-path form emits `$.<path> <op>
    /// <value>`; direct scalar form emits `<op> <value>`. An empty operator
    /// yields `None` (fire on every event).
    #[must_use]
    pub fn build(&self) -> Option<String> {
        if self.op.is_empty() {
            return None;
        }
        let rhs = format!("{} {}", self.op, self.value);
        let rhs = rhs.trim();
        match &self.path {
            None => Some(rhs.to_string()),
            Some(p) if p.trim().is_empty() => Some(rhs.to_string()),
            Some(p) => Some(format!("$.{} {}", p.trim(), rhs)),
        }
    }
}

/// Compare a JSON value against a string comparand using the given operator.
fn compare_values(extracted: &Value, op: ConditionOp, comparand: &str) -> bool {
    // Try numeric comparison first
    if let Some(lhs) = value_as_f64(extracted)
        && let Ok(rhs) = comparand.parse::<f64>()
    {
        return apply_op_f64(lhs, op, rhs);
    }

    // Fall back to string comparison
    let lhs = value_as_string(extracted);
    // Strip surrounding quotes from comparand if present
    let rhs = comparand
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(comparand);

    match op {
        ConditionOp::Eq => lhs == rhs,
        ConditionOp::Neq => lhs != rhs,
        ConditionOp::Gt => lhs.as_str() > rhs,
        ConditionOp::Lt => lhs.as_str() < rhs,
        ConditionOp::Gte => lhs.as_str() >= rhs,
        ConditionOp::Lte => lhs.as_str() <= rhs,
    }
}

fn value_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn value_as_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn apply_op_f64(lhs: f64, op: ConditionOp, rhs: f64) -> bool {
    match op {
        ConditionOp::Gt => lhs > rhs,
        ConditionOp::Lt => lhs < rhs,
        ConditionOp::Gte => lhs >= rhs,
        ConditionOp::Lte => lhs <= rhs,
        ConditionOp::Eq => (lhs - rhs).abs() < f64::EPSILON,
        ConditionOp::Neq => (lhs - rhs).abs() >= f64::EPSILON,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ConditionParts round-trip (cross-surface fixture) ───────────────
    // These fixtures are mirrored verbatim in the web builder test
    // (web/src/lib/sops.condition.test.ts). The two surfaces must parse and
    // build identically or a condition authored in one drifts in the other.

    #[test]
    fn condition_parts_parse_json_path() {
        let p = ConditionParts::parse("$.value >= 85");
        assert_eq!(p.path.as_deref(), Some("value"));
        assert_eq!(p.op, ">=");
        assert_eq!(p.value, "85");
    }

    #[test]
    fn condition_parts_parse_prefers_longest_operator() {
        let p = ConditionParts::parse("$.temp >= 100");
        assert_eq!(p.op, ">=", "must not match bare > before >=");
    }

    #[test]
    fn condition_parts_parse_direct_scalar_has_no_path() {
        let p = ConditionParts::parse("> 0");
        assert_eq!(p.path, None);
        assert_eq!(p.op, ">");
        assert_eq!(p.value, "0");
    }

    #[test]
    fn condition_parts_parse_nested_path() {
        let p = ConditionParts::parse("$.data.temp == critical");
        assert_eq!(p.path.as_deref(), Some("data.temp"));
        assert_eq!(p.op, "==");
        assert_eq!(p.value, "critical");
    }

    #[test]
    fn condition_parts_build_json_path() {
        let p = ConditionParts {
            path: Some("value".into()),
            op: ">=".into(),
            value: "85".into(),
        };
        assert_eq!(p.build().as_deref(), Some("$.value >= 85"));
    }

    #[test]
    fn condition_parts_build_direct_scalar() {
        let p = ConditionParts {
            path: None,
            op: ">".into(),
            value: "0".into(),
        };
        assert_eq!(p.build().as_deref(), Some("> 0"));
    }

    #[test]
    fn condition_parts_empty_op_builds_none() {
        let p = ConditionParts {
            path: Some("value".into()),
            op: String::new(),
            value: "85".into(),
        };
        assert_eq!(p.build(), None);
    }

    #[test]
    fn condition_parts_round_trip_every_operator() {
        for spec in ConditionOp::catalog() {
            let src = format!("$.field {} 5", spec.token);
            let parsed = ConditionParts::parse(&src);
            assert_eq!(parsed.op, spec.token);
            assert_eq!(parsed.build().as_deref(), Some(src.as_str()));
        }
    }

    // ── evaluate_condition (public API) ─────────────────

    #[test]
    fn empty_condition_matches() {
        assert!(evaluate_condition("", Some("anything")));
        assert!(evaluate_condition("  ", None));
    }

    #[test]
    fn missing_payload_fails_closed() {
        assert!(!evaluate_condition("$.value > 85", None));
        assert!(!evaluate_condition("$.value > 85", Some("")));
    }

    // ── JSON path conditions ────────────────────────────

    #[test]
    fn json_path_gt() {
        let payload = r#"{"value": 90}"#;
        assert!(evaluate_condition("$.value > 85", Some(payload)));
        assert!(!evaluate_condition("$.value > 95", Some(payload)));
    }

    #[test]
    fn json_path_gte() {
        let payload = r#"{"value": 85}"#;
        assert!(evaluate_condition("$.value >= 85", Some(payload)));
        assert!(!evaluate_condition("$.value >= 86", Some(payload)));
    }

    #[test]
    fn json_path_lt() {
        let payload = r#"{"temp": 20}"#;
        assert!(evaluate_condition("$.temp < 25", Some(payload)));
        assert!(!evaluate_condition("$.temp < 15", Some(payload)));
    }

    #[test]
    fn json_path_lte() {
        let payload = r#"{"temp": 25}"#;
        assert!(evaluate_condition("$.temp <= 25", Some(payload)));
        assert!(!evaluate_condition("$.temp <= 24", Some(payload)));
    }

    #[test]
    fn json_path_eq() {
        let payload = r#"{"status": "critical"}"#;
        assert!(evaluate_condition(
            r#"$.status == "critical""#,
            Some(payload)
        ));
        assert!(!evaluate_condition(
            r#"$.status == "normal""#,
            Some(payload)
        ));
    }

    #[test]
    fn json_path_neq() {
        let payload = r#"{"status": "ok"}"#;
        assert!(evaluate_condition(r#"$.status != "error""#, Some(payload)));
        assert!(!evaluate_condition(r#"$.status != "ok""#, Some(payload)));
    }

    #[test]
    fn json_path_numeric_eq() {
        let payload = r#"{"count": 42}"#;
        assert!(evaluate_condition("$.count == 42", Some(payload)));
        assert!(!evaluate_condition("$.count == 43", Some(payload)));
    }

    #[test]
    fn json_nested_path() {
        let payload = r#"{"data": {"sensor": {"value": 87.3}}}"#;
        assert!(evaluate_condition(
            "$.data.sensor.value > 85",
            Some(payload)
        ));
        assert!(!evaluate_condition(
            "$.data.sensor.value > 90",
            Some(payload)
        ));
    }

    #[test]
    fn json_path_missing_key() {
        let payload = r#"{"value": 90}"#;
        assert!(!evaluate_condition("$.nonexistent > 0", Some(payload)));
    }

    #[test]
    fn json_invalid_payload() {
        assert!(!evaluate_condition("$.value > 0", Some("not json")));
    }

    #[test]
    fn json_path_array_index() {
        let payload = r#"{"readings": [10, 20, 30]}"#;
        assert!(evaluate_condition("$.readings.1 == 20", Some(payload)));
    }

    #[test]
    fn json_path_bool_value() {
        let payload = r#"{"active": true}"#;
        assert!(evaluate_condition(r#"$.active == "true""#, Some(payload)));
    }

    // ── Direct conditions (peripheral) ──────────────────

    #[test]
    fn direct_gt() {
        assert!(evaluate_condition("> 0", Some("1")));
        assert!(!evaluate_condition("> 0", Some("0")));
        assert!(!evaluate_condition("> 0", Some("-1")));
    }

    #[test]
    fn direct_gte() {
        assert!(evaluate_condition(">= 5", Some("5")));
        assert!(evaluate_condition(">= 5", Some("6")));
        assert!(!evaluate_condition(">= 5", Some("4")));
    }

    #[test]
    fn direct_lt() {
        assert!(evaluate_condition("< 100", Some("50")));
        assert!(!evaluate_condition("< 100", Some("100")));
    }

    #[test]
    fn direct_eq() {
        assert!(evaluate_condition("== 42", Some("42")));
        assert!(!evaluate_condition("== 42", Some("43")));
    }

    #[test]
    fn direct_neq() {
        assert!(evaluate_condition("!= 0", Some("1")));
        assert!(!evaluate_condition("!= 0", Some("0")));
    }

    #[test]
    fn direct_non_numeric_payload() {
        assert!(!evaluate_condition("> 0", Some("not a number")));
    }

    #[test]
    fn direct_float_comparison() {
        assert!(evaluate_condition("> 3.14", Some("3.15")));
        assert!(!evaluate_condition("> 3.14", Some("3.13")));
    }

    // ── Op parsing ──────────────────────────────────────

    #[test]
    fn parse_op_value_basic() {
        let (op, val) = parse_op_value("> 42").unwrap();
        assert_eq!(op, ConditionOp::Gt);
        assert_eq!(val, "42");
    }

    #[test]
    fn parse_op_value_gte_not_gt() {
        let (op, val) = parse_op_value(">= 10").unwrap();
        assert_eq!(op, ConditionOp::Gte);
        assert_eq!(val, "10");
    }

    #[test]
    fn parse_op_value_no_value() {
        assert!(parse_op_value(">").is_none());
        assert!(parse_op_value("> ").is_none());
    }

    #[test]
    fn parse_path_op_value_basic() {
        let (segments, op, val) = parse_path_op_value(".value > 85").unwrap();
        assert_eq!(segments, vec!["value"]);
        assert_eq!(op, ConditionOp::Gt);
        assert_eq!(val, "85");
    }

    #[test]
    fn parse_path_op_value_nested() {
        let (segments, op, val) = parse_path_op_value(".data.temp >= 100").unwrap();
        assert_eq!(segments, vec!["data", "temp"]);
        assert_eq!(op, ConditionOp::Gte);
        assert_eq!(val, "100");
    }

    #[test]
    fn parse_path_op_value_string_comparand() {
        let (segments, op, val) = parse_path_op_value(r#".status == "critical""#).unwrap();
        assert_eq!(segments, vec!["status"]);
        assert_eq!(op, ConditionOp::Eq);
        assert_eq!(val, r#""critical""#);
    }

    // ── resolve_json_path ───────────────────────────────

    #[test]
    fn resolve_path_simple() {
        let json: Value = serde_json::from_str(r#"{"a": 1}"#).unwrap();
        let v = resolve_json_path(&json, &["a"]).unwrap();
        assert_eq!(v, &Value::Number(1.into()));
    }

    #[test]
    fn resolve_path_nested() {
        let json: Value = serde_json::from_str(r#"{"a": {"b": {"c": 42}}}"#).unwrap();
        let v = resolve_json_path(&json, &["a", "b", "c"]).unwrap();
        assert_eq!(v, &Value::Number(42.into()));
    }

    #[test]
    fn resolve_path_missing() {
        let json: Value = serde_json::from_str(r#"{"a": 1}"#).unwrap();
        assert!(resolve_json_path(&json, &["b"]).is_none());
    }

    // ── Operator catalog ────────────────────────────────

    #[test]
    fn op_catalog_covers_every_variant_once() {
        use strum::IntoEnumIterator;
        let catalog = ConditionOp::catalog();
        assert_eq!(
            catalog.len(),
            ConditionOp::iter().count(),
            "catalog must render every operator variant exactly once"
        );
        for op in ConditionOp::iter() {
            assert!(
                catalog.iter().any(|spec| spec.token == op.token()),
                "operator {} missing from catalog",
                op.token()
            );
        }
    }

    #[test]
    fn op_tokens_are_parseable_back() {
        use strum::IntoEnumIterator;
        for op in ConditionOp::iter() {
            let condition = format!("$.value {} 1", op.token());
            // Every catalog token must round-trip through the path parser.
            let parsed = parse_path_op_value(condition.trim_start_matches('$'));
            assert!(
                parsed.is_some(),
                "token {} did not parse back through the grammar",
                op.token()
            );
            assert_eq!(parsed.unwrap().1, op);
        }
    }
}
