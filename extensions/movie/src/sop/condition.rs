use serde_json::Value;

/// Evaluate a trigger condition against an event payload.
///
/// Condition syntax:
///   - JSON path comparison: `$.key.subkey > 85`
///   - Direct numeric comparison: `> 0` (used by peripheral triggers)
///
/// Supported operators: `>=`, `<=`, `!=`, `>`, `<`, `==`
///
/// Returns `false` (fail-closed) when:
///   - payload is missing or empty
///   - condition cannot be parsed
///   - JSON path does not resolve to a value
///   - extracted value and comparand are not comparable
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

/// Operators in order of longest-first to avoid prefix ambiguity.
const OPERATORS: &[&str] = &[">=", "<=", "!=", "==", ">", "<"];

/// Parse `".path.to.field op value"` → `(["path","to","field"], Op, "value")`.
fn parse_path_op_value(input: &str) -> Option<(Vec<&str>, Op, String)> {
    // Input starts after `$`, e.g. `.value > 85` or `.data.temp >= 100`
    // Find operator position
    for &op_str in OPERATORS {
        if let Some(pos) = input.find(op_str) {
            let path_part = input[..pos].trim();
            let value_part = input[pos + op_str.len()..].trim();

            if value_part.is_empty() {
                return None;
            }

            let op = Op::from_str(op_str)?;
            let segments: Vec<&str> = path_part.split('.').filter(|s| !s.is_empty()).collect();

            if segments.is_empty() {
                return None;
            }

            return Some((segments, op, value_part.to_string()));
        }
    }
    None
}

/// Parse `"op value"` → `(Op, "value")`.
fn parse_op_value(input: &str) -> Option<(Op, String)> {
    let input = input.trim();
    for &op_str in OPERATORS {
        if let Some(rest) = input.strip_prefix(op_str) {
            let value = rest.trim();
            if value.is_empty() {
                return None;
            }
            let op = Op::from_str(op_str)?;
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
        if let Ok(idx) = seg.parse::<usize>() {
            if let Some(next) = current.get(idx) {
                current = next;
                continue;
            }
        }
        return None;
    }
    Some(current)
}

// ── Comparison ──────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Gt,
    Lt,
    Gte,
    Lte,
    Eq,
    Neq,
}

impl Op {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            ">" => Some(Self::Gt),
            "<" => Some(Self::Lt),
            ">=" => Some(Self::Gte),
            "<=" => Some(Self::Lte),
            "==" => Some(Self::Eq),
            "!=" => Some(Self::Neq),
            _ => None,
        }
    }
}

/// Compare a JSON value against a string comparand using the given operator.
fn compare_values(extracted: &Value, op: Op, comparand: &str) -> bool {
    // Try numeric comparison first
    if let Some(lhs) = value_as_f64(extracted) {
        if let Ok(rhs) = comparand.parse::<f64>() {
            return apply_op_f64(lhs, op, rhs);
        }
    }

    // Fall back to string comparison
    let lhs = value_as_string(extracted);
    // Strip surrounding quotes from comparand if present
    let rhs = comparand
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(comparand);

    match op {
        Op::Eq => lhs == rhs,
        Op::Neq => lhs != rhs,
        Op::Gt => lhs.as_str() > rhs,
        Op::Lt => lhs.as_str() < rhs,
        Op::Gte => lhs.as_str() >= rhs,
        Op::Lte => lhs.as_str() <= rhs,
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

fn apply_op_f64(lhs: f64, op: Op, rhs: f64) -> bool {
    match op {
        Op::Gt => lhs > rhs,
        Op::Lt => lhs < rhs,
        Op::Gte => lhs >= rhs,
        Op::Lte => lhs <= rhs,
        Op::Eq => (lhs - rhs).abs() < f64::EPSILON,
        Op::Neq => (lhs - rhs).abs() >= f64::EPSILON,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(op, Op::Gt);
        assert_eq!(val, "42");
    }

    #[test]
    fn parse_op_value_gte_not_gt() {
        let (op, val) = parse_op_value(">= 10").unwrap();
        assert_eq!(op, Op::Gte);
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
        assert_eq!(op, Op::Gt);
        assert_eq!(val, "85");
    }

    #[test]
    fn parse_path_op_value_nested() {
        let (segments, op, val) = parse_path_op_value(".data.temp >= 100").unwrap();
        assert_eq!(segments, vec!["data", "temp"]);
        assert_eq!(op, Op::Gte);
        assert_eq!(val, "100");
    }

    #[test]
    fn parse_path_op_value_string_comparand() {
        let (segments, op, val) = parse_path_op_value(r#".status == "critical""#).unwrap();
        assert_eq!(segments, vec!["status"]);
        assert_eq!(op, Op::Eq);
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
}
