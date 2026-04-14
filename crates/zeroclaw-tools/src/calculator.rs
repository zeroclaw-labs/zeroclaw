use async_trait::async_trait;
use serde_json::json;
use zeroclaw_api::tool::{Tool, ToolResult};

pub struct CalculatorTool;

impl CalculatorTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CalculatorTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for CalculatorTool {
    fn name(&self) -> &str {
        "calculator"
    }

    fn description(&self) -> &str {
        "Perform arithmetic and statistical calculations. Supports 25 functions: \
         add, subtract, divide, multiply, pow, sqrt, abs, modulo, round, \
         log, ln, exp, factorial, sum, average, median, mode, min, max, \
         range, variance, stdev, percentile, count, percentage_change, clamp. \
         Use this tool whenever you need to compute a numeric result instead of guessing."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "function": {
                    "type": "string",
                    "description": "Calculation to perform. \
                        Arithmetic: add(values), subtract(values), divide(values), multiply(values), pow(a,b), sqrt(x), abs(x), modulo(a,b), round(x,decimals). \
                        Logarithmic/exponential: log(x,base?), ln(x), exp(x), factorial(x). \
                        Aggregation: sum(values), average(values), count(values), min(values), max(values), range(values). \
                        Statistics: median(values), mode(values), variance(values), stdev(values), percentile(values,p). \
                        Utility: percentage_change(a,b), clamp(x,min_val,max_val).",
                    "enum": [
                        "add", "subtract", "divide", "multiply", "pow", "sqrt",
                        "abs", "modulo", "round", "log", "ln", "exp", "factorial",
                        "sum", "average", "median", "mode", "min", "max", "range",
                        "variance", "stdev", "percentile", "count",
                        "percentage_change", "clamp"
                    ]
                },
                "values": {
                    "type": "array",
                    "items": { "type": "number" },
                    "description": "Array of numeric values. Required for: add, subtract, divide, multiply, sum, average, median, mode, min, max, range, variance, stdev, percentile, count."
                },
                "a": {
                    "type": "number",
                    "description": "First operand. Required for: pow, modulo, percentage_change."
                },
                "b": {
                    "type": "number",
                    "description": "Second operand. Required for: pow, modulo, percentage_change."
                },
                "x": {
                    "type": "number",
                    "description": "Input number. Required for: sqrt, abs, exp, ln, log, factorial."
                },
                "base": {
                    "type": "number",
                    "description": "Logarithm base (default: 10). Optional for: log."
                },
                "decimals": {
                    "type": "integer",
                    "description": "Number of decimal places for rounding. Required for: round."
                },
                "p": {
                    "type": "integer",
                    "description": "Percentile rank (0-100). Required for: percentile."
                },
                "min_val": {
                    "type": "number",
                    "description": "Minimum bound. Required for: clamp."
                },
                "max_val": {
                    "type": "number",
                    "description": "Maximum bound. Required for: clamp."
                }
            },
            "required": ["function"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let function = match args.get("function").and_then(|v| v.as_str()) {
            Some(f) => f,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing required parameter: function".to_string()),
                });
            }
        };

        let result = match function {
            "add" => calc_add(&args),
            "subtract" => calc_subtract(&args),
            "divide" => calc_divide(&args),
            "multiply" => calc_multiply(&args),
            "pow" => calc_pow(&args),
            "sqrt" => calc_sqrt(&args),
            "abs" => calc_abs(&args),
            "modulo" => calc_modulo(&args),
            "round" => calc_round(&args),
            "log" => calc_log(&args),
            "ln" => calc_ln(&args),
            "exp" => calc_exp(&args),
            "factorial" => calc_factorial(&args),
            "sum" => calc_sum(&args),
            "average" => calc_average(&args),
            "median" => calc_median(&args),
            "mode" => calc_mode(&args),
            "min" => calc_min(&args),
            "max" => calc_max(&args),
            "range" => calc_range(&args),
            "variance" => calc_variance(&args),
            "stdev" => calc_stdev(&args),
            "percentile" => calc_percentile(&args),
            "count" => calc_count(&args),
            "percentage_change" => calc_percentage_change(&args),
            "clamp" => calc_clamp(&args),
            other => Err(format!("Unknown function: {other}")),
        };

        match result {
            Ok(output) => Ok(ToolResult {
                success: true,
                output,
                error: None,
            }),
            Err(err) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(err),
            }),
        }
    }
}

fn extract_f64(args: &serde_json::Value, key: &str, name: &str) -> Result<f64, String> {
    args.get(key)
        .and_then(|v| v.as_f64())
        .ok_or_else(|| format!("Missing required parameter: {name}"))
}

fn extract_i64(args: &serde_json::Value, key: &str, name: &str) -> Result<i64, String> {
    args.get(key)
        .and_then(|v| v.as_i64())
        .ok_or_else(|| format!("Missing required parameter: {name}"))
}

fn extract_values(args: &serde_json::Value, min_len: usize) -> Result<Vec<f64>, String> {
    let values = args
        .get("values")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Missing required parameter: values (array of numbers)".to_string())?;
    if values.len() < min_len {
        return Err(format!(
            "Expected at least {min_len} value(s), got {}",
            values.len()
        ));
    }
    let mut nums = Vec::with_capacity(values.len());
    for (i, v) in values.iter().enumerate() {
        match v.as_f64() {
            Some(n) => nums.push(n),
            None => return Err(format!("values[{i}] is not a valid number")),
        }
    }
    Ok(nums)
}

fn format_num(n: f64) -> String {
    if n == n.floor() && n.abs() < 1e15 {
        #[allow(clippy::cast_possible_truncation)]
        let rounded = n.round() as i128;
        format!("{rounded}")
    } else {
        format!("{n}")
    }
}

fn calc_add(args: &serde_json::Value) -> Result<String, String> {
    let values = extract_values(args, 2)?;
    Ok(format_num(values.iter().sum()))
}

fn calc_subtract(args: &serde_json::Value) -> Result<String, String> {
    let values = extract_values(args, 2)?;
    let mut iter = values.iter();
    let mut result = *iter.next().unwrap();
    for v in iter {
        result -= v;
    }
    Ok(format_num(result))
}

fn calc_divide(args: &serde_json::Value) -> Result<String, String> {
    let values = extract_values(args, 2)?;
    let mut iter = values.iter();
    let mut result = *iter.next().unwrap();
    for v in iter {
        if *v == 0.0 {
            return Err("Division by zero".to_string());
        }
        result /= v;
    }
    Ok(format_num(result))
}

fn calc_multiply(args: &serde_json::Value) -> Result<String, String> {
    let values = extract_values(args, 2)?;
    let mut result = 1.0;
    for v in &values {
        result *= v;
    }
    Ok(format_num(result))
}

fn calc_pow(args: &serde_json::Value) -> Result<String, String> {
    let base = extract_f64(args, "a", "a (base)")?;
    let exp = extract_f64(args, "b", "b (exponent)")?;
    Ok(format_num(base.powf(exp)))
}

fn calc_sqrt(args: &serde_json::Value) -> Result<String, String> {
    let x = extract_f64(args, "x", "x")?;
    if x < 0.0 {
        return Err("Cannot compute square root of a negative number".to_string());
    }
    Ok(format_num(x.sqrt()))
}

fn calc_abs(args: &serde_json::Value) -> Result<String, String> {
    let x = extract_f64(args, "x", "x")?;
    Ok(format_num(x.abs()))
}

fn calc_modulo(args: &serde_json::Value) -> Result<String, String> {
    let a = extract_f64(args, "a", "a")?;
    let b = extract_f64(args, "b", "b")?;
    if b == 0.0 {
        return Err("Modulo by zero".to_string());
    }
    Ok(format_num(a % b))
}

fn calc_round(args: &serde_json::Value) -> Result<String, String> {
    let x = extract_f64(args, "x", "x")?;
    let decimals = extract_i64(args, "decimals", "decimals")?;
    if decimals < 0 {
        return Err("decimals must be non-negative".to_string());
    }
    let multiplier = 10_f64.powi(i32::try_from(decimals).unwrap_or(i32::MAX));
    Ok(format_num((x * multiplier).round() / multiplier))
}

fn calc_log(args: &serde_json::Value) -> Result<String, String> {
    let x = extract_f64(args, "x", "x")?;
    if x <= 0.0 {
        return Err("Logarithm requires a positive number".to_string());
    }
    let base = args.get("base").and_then(|v| v.as_f64()).unwrap_or(10.0);
    if base <= 0.0 || base == 1.0 {
        return Err("Logarithm base must be positive and not equal to 1".to_string());
    }
    Ok(format_num(x.log(base)))
}

fn calc_ln(args: &serde_json::Value) -> Result<String, String> {
    let x = extract_f64(args, "x", "x")?;
    if x <= 0.0 {
        return Err("Natural logarithm requires a positive number".to_string());
    }
    Ok(format_num(x.ln()))
}

fn calc_exp(args: &serde_json::Value) -> Result<String, String> {
    let x = extract_f64(args, "x", "x")?;
    Ok(format_num(x.exp()))
}

fn calc_factorial(args: &serde_json::Value) -> Result<String, String> {
    let x = extract_f64(args, "x", "x")?;
    if x < 0.0 || x != x.floor() {
        return Err("Factorial requires a non-negative integer".to_string());
    }
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let n = x.round() as u128;
    if n > 170 {
        return Err("Factorial result exceeds f64 range (max input: 170)".to_string());
    }
    let mut result: u128 = 1;
    for i in 2..=n {
        result *= i;
    }
    Ok(result.to_string())
}

fn calc_sum(args: &serde_json::Value) -> Result<String, String> {
    let values = extract_values(args, 1)?;
    Ok(format_num(values.iter().sum()))
}

fn calc_average(args: &serde_json::Value) -> Result<String, String> {
    let values = extract_values(args, 1)?;
    if values.is_empty() {
        return Err("Cannot compute average of an empty array".to_string());
    }
    Ok(format_num(values.iter().sum::<f64>() / values.len() as f64))
}

fn calc_median(args: &serde_json::Value) -> Result<String, String> {
    let mut values = extract_values(args, 1)?;
    if values.is_empty() {
        return Err("Cannot compute median of an empty array".to_string());
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let len = values.len();
    if len % 2 == 0 {
        Ok(format_num(f64::midpoint(
            values[len / 2 - 1],
            values[len / 2],
        )))
    } else {
        Ok(format_num(values[len / 2]))
    }
}

fn calc_mode(args: &serde_json::Value) -> Result<String, String> {
    let values = extract_values(args, 1)?;
    if values.is_empty() {
        return Err("Cannot compute mode of an empty array".to_string());
    }
    let mut freq: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    for &v in &values {
        let key = v.to_bits();
        *freq.entry(key).or_insert(0) += 1;
    }
    let max_freq = *freq.values().max().unwrap();
    let mut seen = std::collections::HashSet::new();
    let mut modes = Vec::new();
    for &v in &values {
        let key = v.to_bits();
        if freq[&key] == max_freq && seen.insert(key) {
            modes.push(v);
        }
    }
    if modes.len() == 1 {
        Ok(format_num(modes[0]))
    } else {
        let formatted: Vec<String> = modes.iter().map(|v| format_num(*v)).collect();
        Ok(format!("Modes: {}", formatted.join(", ")))
    }
}

fn calc_min(args: &serde_json::Value) -> Result<String, String> {
    let values = extract_values(args, 1)?;
    let Some(min_val) = values.iter().copied().reduce(f64::min) else {
        return Err("Cannot compute min of an empty array".to_string());
    };
    Ok(format_num(min_val))
}

fn calc_max(args: &serde_json::Value) -> Result<String, String> {
    let values = extract_values(args, 1)?;
    let Some(max_val) = values.iter().copied().reduce(f64::max) else {
        return Err("Cannot compute max of an empty array".to_string());
    };
    Ok(format_num(max_val))
}

fn calc_range(args: &serde_json::Value) -> Result<String, String> {
    let values = extract_values(args, 1)?;
    if values.is_empty() {
        return Err("Cannot compute range of an empty array".to_string());
    }
    let min_val = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max_val = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    Ok(format_num(max_val - min_val))
}

fn calc_variance(args: &serde_json::Value) -> Result<String, String> {
    let values = extract_values(args, 1)?;
    if values.len() < 2 {
        return Err("Variance requires at least 2 values".to_string());
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64;
    Ok(format_num(variance))
}

fn calc_stdev(args: &serde_json::Value) -> Result<String, String> {
    let values = extract_values(args, 1)?;
    if values.len() < 2 {
        return Err("Standard deviation requires at least 2 values".to_string());
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64;
    Ok(format_num(variance.sqrt()))
}

fn calc_percentile(args: &serde_json::Value) -> Result<String, String> {
    let mut values = extract_values(args, 1)?;
    if values.is_empty() {
        return Err("Cannot compute percentile of an empty array".to_string());
    }
    let p = extract_i64(args, "p", "p (percentile rank 0-100)")?;
    if !(0..=100).contains(&p) {
        return Err("Percentile rank must be between 0 and 100".to_string());
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let idx_f = p as f64 / 100.0 * (values.len() - 1) as f64;
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let index = idx_f.round().clamp(0.0, (values.len() - 1) as f64) as usize;
    Ok(format_num(values[index]))
}

fn calc_count(args: &serde_json::Value) -> Result<String, String> {
    let values = extract_values(args, 1)?;
    Ok(values.len().to_string())
}

fn calc_percentage_change(args: &serde_json::Value) -> Result<String, String> {
    let old = extract_f64(args, "a", "a (old value)")?;
    let new = extract_f64(args, "b", "b (new value)")?;
    if old == 0.0 {
        return Err("Cannot compute percentage change from zero".to_string());
    }
    Ok(format_num((new - old) / old.abs() * 100.0))
}

fn calc_clamp(args: &serde_json::Value) -> Result<String, String> {
    let x = extract_f64(args, "x", "x")?;
    let min_val = extract_f64(args, "min_val", "min_val")?;
    let max_val = extract_f64(args, "max_val", "max_val")?;
    if min_val > max_val {
        return Err("min_val must be less than or equal to max_val".to_string());
    }
    Ok(format_num(x.clamp(min_val, max_val)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_add() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "add", "values": [1.0, 2.0, 3.5]}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "6.5");
    }

    #[tokio::test]
    async fn test_subtract() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "subtract", "values": [10.0, 3.0, 1.5]}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "5.5");
    }

    #[tokio::test]
    async fn test_divide() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "divide", "values": [100.0, 4.0]}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "25");
    }

    #[tokio::test]
    async fn test_divide_by_zero() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "divide", "values": [10.0, 0.0]}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("zero"));
    }

    #[tokio::test]
    async fn test_multiply() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "multiply", "values": [3.0, 4.0, 5.0]}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "60");
    }

    #[tokio::test]
    async fn test_pow() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "pow", "a": 2.0, "b": 10.0}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "1024");
    }

    #[tokio::test]
    async fn test_sqrt() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "sqrt", "x": 144.0}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "12");
    }

    #[tokio::test]
    async fn test_sqrt_negative() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "sqrt", "x": -4.0}))
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn test_abs() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "abs", "x": -42.5}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "42.5");
    }

    #[tokio::test]
    async fn test_modulo() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "modulo", "a": 17.0, "b": 5.0}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "2");
    }

    #[tokio::test]
    async fn test_round() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "round", "x": 2.715, "decimals": 2}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "2.72");
    }

    #[tokio::test]
    async fn test_log_base10() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "log", "x": 100.0}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "2");
    }

    #[tokio::test]
    async fn test_log_custom_base() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "log", "x": 8.0, "base": 2.0}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "3");
    }

    #[tokio::test]
    async fn test_ln() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "ln", "x": 1.0}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "0");
    }

    #[tokio::test]
    async fn test_exp() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "exp", "x": 0.0}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "1");
    }

    #[tokio::test]
    async fn test_factorial() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "factorial", "x": 5.0}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "120");
    }

    #[tokio::test]
    async fn test_average() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "average", "values": [10.0, 20.0, 30.0]}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "20");
    }

    #[tokio::test]
    async fn test_median_odd() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "median", "values": [3.0, 1.0, 2.0]}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "2");
    }

    #[tokio::test]
    async fn test_median_even() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "median", "values": [4.0, 1.0, 3.0, 2.0]}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "2.5");
    }

    #[tokio::test]
    async fn test_mode() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "mode", "values": [1.0, 2.0, 2.0, 3.0, 3.0, 3.0]}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "3");
    }

    #[tokio::test]
    async fn test_min() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "min", "values": [5.0, 2.0, 8.0, 1.0]}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "1");
    }

    #[tokio::test]
    async fn test_max() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "max", "values": [5.0, 2.0, 8.0, 1.0]}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "8");
    }

    #[tokio::test]
    async fn test_range() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "range", "values": [1.0, 5.0, 10.0]}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "9");
    }

    #[tokio::test]
    async fn test_variance() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(
                json!({"function": "variance", "values": [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0]}),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "4");
    }

    #[tokio::test]
    async fn test_stdev() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(
                json!({"function": "stdev", "values": [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0]}),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "2");
    }

    #[tokio::test]
    async fn test_percentile_50() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(
                json!({"function": "percentile", "values": [1.0, 2.0, 3.0, 4.0, 5.0], "p": 50}),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "3");
    }

    #[tokio::test]
    async fn test_count() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "count", "values": [1.0, 2.0, 3.0, 4.0, 5.0]}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "5");
    }

    #[tokio::test]
    async fn test_percentage_change() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "percentage_change", "a": 50.0, "b": 75.0}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "50");
    }

    #[tokio::test]
    async fn test_clamp_within_range() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "clamp", "x": 5.0, "min_val": 1.0, "max_val": 10.0}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "5");
    }

    #[tokio::test]
    async fn test_clamp_below_min() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "clamp", "x": -5.0, "min_val": 0.0, "max_val": 10.0}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "0");
    }

    #[tokio::test]
    async fn test_clamp_above_max() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "clamp", "x": 15.0, "min_val": 0.0, "max_val": 10.0}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "10");
    }

    #[tokio::test]
    async fn test_unknown_function() {
        let tool = CalculatorTool::new();
        let result = tool.execute(json!({"function": "unknown"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("Unknown function"));
    }

    #[tokio::test]
    async fn test_sum() {
        let tool = CalculatorTool::new();
        let result = tool
            .execute(json!({"function": "sum", "values": [1.0, 2.0, 3.0, 4.0, 5.0]}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "15");
    }
}
