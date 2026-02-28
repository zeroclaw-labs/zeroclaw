//! __SKILL_NAME__ — ZeroClaw Skill (Rust / WASI)
//!
//! Returns mock weather data for a given city.
//! Protocol: read JSON from stdin, write JSON result to stdout.
//! Build:    cargo build --target wasm32-wasip1 --release
//!           cp target/wasm32-wasip1/release/__BIN_NAME__.wasm tool.wasm
//! Test:     zeroclaw skill test . --args '{"city":"hanoi"}'

use std::io::{self, Read, Write};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct Args {
    city: String,
}

#[derive(Serialize)]
struct WeatherData {
    city: String,
    temperature_c: f32,
    condition: String,
    humidity_pct: u8,
    wind_kmh: u8,
}

#[derive(Serialize)]
struct ToolResult {
    success: bool,
    output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<WeatherData>,
}

fn write_result(r: &ToolResult) {
    let out = serde_json::to_string(r)
        .unwrap_or_else(|_| r#"{"success":false,"output":"","error":"serialization error"}"#.to_string());
    let _ = io::stdout().write_all(out.as_bytes());
}

fn main() {
    let mut buf = String::new();
    if let Err(e) = io::stdin().read_to_string(&mut buf) {
        write_result(&ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!("failed to read stdin: {e}")),
            data: None,
        });
        return;
    }

    let result = match serde_json::from_str::<Args>(&buf) {
        Ok(args) => lookup_weather(&args.city),
        Err(e) => ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!("invalid input: {e} — expected {{\"city\": \"<name>\"}}")),
            data: None,
        },
    };

    write_result(&result);
}

fn lookup_weather(city: &str) -> ToolResult {
    // Mock weather database — no HTTP inside WASI sandbox
    let weather = match city.to_lowercase().as_str() {
        "hanoi" | "ha noi" => WeatherData {
            city: "Hanoi".into(),
            temperature_c: 28.5,
            condition: "Partly Cloudy".into(),
            humidity_pct: 75,
            wind_kmh: 12,
        },
        "ho chi minh" | "hcm" | "saigon" => WeatherData {
            city: "Ho Chi Minh City".into(),
            temperature_c: 33.0,
            condition: "Sunny".into(),
            humidity_pct: 68,
            wind_kmh: 8,
        },
        "da nang" => WeatherData {
            city: "Da Nang".into(),
            temperature_c: 30.2,
            condition: "Clear".into(),
            humidity_pct: 65,
            wind_kmh: 15,
        },
        "london" => WeatherData {
            city: "London".into(),
            temperature_c: 12.0,
            condition: "Overcast".into(),
            humidity_pct: 82,
            wind_kmh: 20,
        },
        "tokyo" => WeatherData {
            city: "Tokyo".into(),
            temperature_c: 18.0,
            condition: "Light Rain".into(),
            humidity_pct: 78,
            wind_kmh: 10,
        },
        "new york" | "nyc" => WeatherData {
            city: "New York".into(),
            temperature_c: 15.0,
            condition: "Cloudy".into(),
            humidity_pct: 70,
            wind_kmh: 18,
        },
        "paris" => WeatherData {
            city: "Paris".into(),
            temperature_c: 14.5,
            condition: "Rainy".into(),
            humidity_pct: 85,
            wind_kmh: 22,
        },
        "singapore" => WeatherData {
            city: "Singapore".into(),
            temperature_c: 31.0,
            condition: "Thunderstorm".into(),
            humidity_pct: 88,
            wind_kmh: 14,
        },
        _ => {
            return ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "city '{city}' not found. Supported: Hanoi, Ho Chi Minh, Da Nang, London, Tokyo, New York, Paris, Singapore"
                )),
                data: None,
            };
        }
    };

    let output = format!(
        "{}: {}°C, {}, humidity {}%, wind {} km/h",
        weather.city, weather.temperature_c, weather.condition, weather.humidity_pct, weather.wind_kmh
    );

    ToolResult { success: true, output, error: None, data: Some(weather) }
}
