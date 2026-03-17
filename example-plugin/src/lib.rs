//! Example ZeroClaw weather plugin.
//!
//! Demonstrates how to create a WASM tool plugin using extism-pdk.
//! Build with: cargo build --target wasm32-wasip1 --release

use extism_pdk::*;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct WeatherInput {
    location: String,
}

#[derive(Serialize)]
struct WeatherOutput {
    location: String,
    temperature: f64,
    unit: String,
    condition: String,
    humidity: u32,
}

/// Get weather for a location (mock implementation for demonstration).
#[plugin_fn]
pub fn get_weather(input: String) -> FnResult<String> {
    let params: WeatherInput =
        serde_json::from_str(&input).map_err(|e| Error::msg(format!("invalid input: {e}")))?;

    // Mock weather data for demonstration
    let output = WeatherOutput {
        location: params.location,
        temperature: 22.5,
        unit: "celsius".to_string(),
        condition: "Partly cloudy".to_string(),
        humidity: 65,
    };

    let json = serde_json::to_string(&output)
        .map_err(|e| Error::msg(format!("serialization error: {e}")))?;

    Ok(json)
}
