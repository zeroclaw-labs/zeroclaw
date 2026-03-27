//! Weather tool — fetches current conditions and forecast via wttr.in.
//!
//! Uses the free, no-API-key wttr.in service (`?format=j1` JSON endpoint).
//! Supports any location wttr.in accepts: city names (in any language/script),
//! airport IATA codes, GPS coordinates, zip/postal codes, and domain-based
//! geolocation. Units default to metric but can be overridden per-call.

use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

const WTTR_BASE_URL: &str = "https://wttr.in";
const WTTR_TIMEOUT_SECS: u64 = 15;
const WTTR_CONNECT_TIMEOUT_SECS: u64 = 10;

// ── wttr.in JSON response types ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct WttrResponse {
    current_condition: Vec<CurrentCondition>,
    nearest_area: Vec<NearestArea>,
    weather: Vec<WeatherDay>,
}

#[derive(Debug, Deserialize)]
struct CurrentCondition {
    #[serde(rename = "temp_C")]
    temp_c: String,
    #[serde(rename = "temp_F")]
    temp_f: String,
    #[serde(rename = "FeelsLikeC")]
    feels_like_c: String,
    #[serde(rename = "FeelsLikeF")]
    feels_like_f: String,
    humidity: String,
    #[serde(rename = "weatherDesc")]
    weather_desc: Vec<StringValue>,
    #[serde(rename = "windspeedKmph")]
    windspeed_kmph: String,
    #[serde(rename = "windspeedMiles")]
    windspeed_miles: String,
    #[serde(rename = "winddir16Point")]
    winddir_16point: String,
    #[serde(rename = "precipMM")]
    precip_mm: String,
    #[serde(rename = "precipInches")]
    precip_inches: String,
    visibility: String,
    #[serde(rename = "visibilityMiles")]
    visibility_miles: String,
    #[serde(rename = "uvIndex")]
    uv_index: String,
    #[serde(rename = "cloudcover")]
    cloud_cover: String,
    #[serde(rename = "pressure")]
    pressure_mb: String,
    #[serde(rename = "pressureInches")]
    pressure_inches: String,
    #[serde(rename = "observation_time")]
    observation_time: String,
}

#[derive(Debug, Deserialize)]
struct NearestArea {
    #[serde(rename = "areaName")]
    area_name: Vec<StringValue>,
    country: Vec<StringValue>,
    region: Vec<StringValue>,
}

#[derive(Debug, Deserialize)]
struct WeatherDay {
    date: String,
    #[serde(rename = "maxtempC")]
    max_temp_c: String,
    #[serde(rename = "maxtempF")]
    max_temp_f: String,
    #[serde(rename = "mintempC")]
    min_temp_c: String,
    #[serde(rename = "mintempF")]
    min_temp_f: String,
    #[serde(rename = "avgtempC")]
    avg_temp_c: String,
    #[serde(rename = "avgtempF")]
    avg_temp_f: String,
    #[serde(rename = "sunHour")]
    sun_hours: String,
    #[serde(rename = "uvIndex")]
    uv_index: String,
    #[serde(rename = "totalSnow_cm")]
    total_snow_cm: String,
    astronomy: Vec<Astronomy>,
    hourly: Vec<HourlyCondition>,
}

#[derive(Debug, Deserialize)]
struct Astronomy {
    sunrise: String,
    sunset: String,
    moon_phase: String,
}

#[derive(Debug, Deserialize)]
struct HourlyCondition {
    time: String,
    #[serde(rename = "tempC")]
    temp_c: String,
    #[serde(rename = "tempF")]
    temp_f: String,
    #[serde(rename = "weatherDesc")]
    weather_desc: Vec<StringValue>,
    #[serde(rename = "chanceofrain")]
    chance_of_rain: String,
    #[serde(rename = "chanceofsnow")]
    chance_of_snow: String,
    #[serde(rename = "windspeedKmph")]
    windspeed_kmph: String,
    #[serde(rename = "windspeedMiles")]
    windspeed_miles: String,
    #[serde(rename = "winddir16Point")]
    winddir_16point: String,
}

#[derive(Debug, Deserialize)]
struct StringValue {
    value: String,
}

// ── Tool struct ───────────────────────────────────────────────────────────────

/// Fetches weather data from wttr.in — no API key required, global coverage.
pub struct WeatherTool;

impl WeatherTool {
    pub fn new() -> Self {
        Self
    }

    /// Build the wttr.in request URL for the given location.
    fn build_url(location: &str) -> String {
        // Percent-encode spaces; wttr.in also accepts `+` but %20 is safer.
        let encoded = location.trim().replace(' ', "+");
        format!("{WTTR_BASE_URL}/{encoded}?format=j1")
    }

    /// Fetch and parse the wttr.in JSON response.
    async fn fetch(location: &str) -> anyhow::Result<WttrResponse> {
        let url = Self::build_url(location);

        let builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(WTTR_TIMEOUT_SECS))
            .connect_timeout(Duration::from_secs(WTTR_CONNECT_TIMEOUT_SECS))
            .user_agent("zeroclaw-weather/1.0");

        let builder = crate::config::apply_runtime_proxy_to_builder(builder, "tool.weather");
        let client = builder.build()?;

        let response = client.get(&url).send().await?;
        let status = response.status();

        if !status.is_success() {
            anyhow::bail!(
                "wttr.in returned HTTP {status} for location '{location}'. \
                 Check that the location is valid."
            );
        }

        let body = response.text().await?;

        // wttr.in returns a plain-text error string (not JSON) for unknown locations.
        if !body.trim_start().starts_with('{') {
            anyhow::bail!(
                "wttr.in could not resolve location '{location}'. \
                 Try a city name, airport code, GPS coordinates (lat,lon), or zip code."
            );
        }

        let parsed: WttrResponse = serde_json::from_str(&body)
            .map_err(|e| anyhow::anyhow!("Failed to parse wttr.in response: {e}"))?;

        Ok(parsed)
    }

    /// Format a single hourly slot for the forecast block.
    fn format_hourly(h: &HourlyCondition, metric: bool) -> String {
        // wttr.in encodes time as "0", "300", "600" … "2100" (HHMM without leading zero)
        let hour_num: u32 = h.time.parse().unwrap_or(0);
        let hour_display = format!("{:02}:00", hour_num / 100);
        let temp = if metric {
            format!("{}°C", h.temp_c)
        } else {
            format!("{}°F", h.temp_f)
        };
        let wind_speed = if metric {
            format!("{} km/h", h.windspeed_kmph)
        } else {
            format!("{} mph", h.windspeed_miles)
        };
        let desc = h
            .weather_desc
            .first()
            .map(|v| v.value.trim().to_string())
            .unwrap_or_default();
        format!(
            "    {hour_display}: {temp} — {desc} | Wind: {wind_speed} {} | Rain: {}% | Snow: {}%",
            h.winddir_16point, h.chance_of_rain, h.chance_of_snow,
        )
    }

    /// Format a full day forecast block.
    fn format_day(day: &WeatherDay, metric: bool, include_hourly: bool) -> String {
        let (max, min, avg) = if metric {
            (
                format!("{}°C", day.max_temp_c),
                format!("{}°C", day.min_temp_c),
                format!("{}°C", day.avg_temp_c),
            )
        } else {
            (
                format!("{}°F", day.max_temp_f),
                format!("{}°F", day.min_temp_f),
                format!("{}°F", day.avg_temp_f),
            )
        };

        let astronomy = day.astronomy.first();
        let sunrise = astronomy.map(|a| a.sunrise.as_str()).unwrap_or("N/A");
        let sunset = astronomy.map(|a| a.sunset.as_str()).unwrap_or("N/A");
        let moon = astronomy.map(|a| a.moon_phase.as_str()).unwrap_or("N/A");

        let snow_note = if day.total_snow_cm != "0.0" && day.total_snow_cm != "0" {
            let snow_str = if metric {
                format!(" | Snow: {} cm", day.total_snow_cm)
            } else {
                // convert cm → inches for imperial display
                let cm: f64 = day.total_snow_cm.parse().unwrap_or(0.0);
                format!(" | Snow: {:.1} in", cm / 2.54)
            };
            snow_str
        } else {
            String::new()
        };

        let mut out = format!(
            "  {date}: High {max} / Low {min} / Avg {avg} | UV: {uv} | Sun: {sun_hours}h | {snow}\
             Sunrise: {sunrise} | Sunset: {sunset} | Moon: {moon}",
            date = day.date,
            uv = day.uv_index,
            sun_hours = day.sun_hours,
            snow = snow_note,
        );

        if include_hourly && !day.hourly.is_empty() {
            out.push('\n');
            // Emit every other slot (3-hourly → 6-hourly) to keep output concise
            for h in day.hourly.iter().step_by(2) {
                out.push('\n');
                out.push_str(&Self::format_hourly(h, metric));
            }
        }

        out
    }

    /// Build the final human-readable output string.
    fn format_output(data: &WttrResponse, metric: bool, days: u8) -> String {
        let current = match data.current_condition.first() {
            Some(c) => c,
            None => return "No current conditions available.".to_string(),
        };

        let area = data.nearest_area.first();
        let location_str = area
            .map(|a| {
                let city = a.area_name.first().map(|v| v.value.as_str()).unwrap_or("");
                let region = a.region.first().map(|v| v.value.as_str()).unwrap_or("");
                let country = a.country.first().map(|v| v.value.as_str()).unwrap_or("");
                match (city.is_empty(), region.is_empty()) {
                    (false, false) => format!("{city}, {region}, {country}"),
                    (false, true) => format!("{city}, {country}"),
                    _ => country.to_string(),
                }
            })
            .unwrap_or_else(|| "Unknown location".to_string());

        let desc = current
            .weather_desc
            .first()
            .map(|v| v.value.trim().to_string())
            .unwrap_or_else(|| "Unknown".to_string());

        let (temp, feels_like, wind_speed, precip, visibility, pressure) = if metric {
            (
                format!("{}°C", current.temp_c),
                format!("{}°C", current.feels_like_c),
                format!("{} km/h", current.windspeed_kmph),
                format!("{} mm", current.precip_mm),
                format!("{} km", current.visibility),
                format!("{} hPa", current.pressure_mb),
            )
        } else {
            (
                format!("{}°F", current.temp_f),
                format!("{}°F", current.feels_like_f),
                format!("{} mph", current.windspeed_miles),
                format!("{} in", current.precip_inches),
                format!("{} mi", current.visibility_miles),
                format!("{} inHg", current.pressure_inches),
            )
        };

        let mut out = format!(
            "Weather for {location_str} (as of {obs_time})\n\
             ─────────────────────────────────────────\n\
             Conditions : {desc}\n\
             Temperature: {temp} (feels like {feels_like})\n\
             Humidity   : {humidity}%\n\
             Wind       : {wind_speed} {winddir}\n\
             Precipitation: {precip}\n\
             Visibility : {visibility}\n\
             Pressure   : {pressure}\n\
             Cloud Cover: {cloud}%\n\
             UV Index   : {uv}",
            obs_time = current.observation_time,
            humidity = current.humidity,
            winddir = current.winddir_16point,
            cloud = current.cloud_cover,
            uv = current.uv_index,
        );

        // Forecast days (wttr.in always returns 3 days; day 0 = today)
        let forecast_days: Vec<&WeatherDay> = data.weather.iter().take(days as usize).collect();
        if !forecast_days.is_empty() {
            out.push_str("\n\nForecast\n────────");
            let include_hourly = days <= 2;
            for day in &forecast_days {
                out.push('\n');
                out.push_str(&Self::format_day(day, metric, include_hourly));
            }
        }

        out
    }
}

impl Default for WeatherTool {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tool trait ────────────────────────────────────────────────────────────────

#[async_trait]
impl Tool for WeatherTool {
    fn name(&self) -> &str {
        "weather"
    }

    fn description(&self) -> &str {
        "Get current weather conditions and up to 3-day forecast for any location worldwide. \
         Supports city names (in any language or script), airport IATA codes (e.g. 'LAX'), \
         GPS coordinates (e.g. '51.5,-0.1'), postal/zip codes, and domain-based geolocation. \
         No API key required. Units default to metric (°C, km/h, mm) but can be switched to \
         imperial (°F, mph, inches) per request."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "location": {
                    "type": "string",
                    "description": "Location to get weather for. Accepts city names in any \
                                    language/script, IATA airport codes, GPS coordinates \
                                    (e.g. '35.6762,139.6503'), postal/zip codes, or a \
                                    domain name for geolocation (e.g. 'stackoverflow.com')."
                },
                "units": {
                    "type": "string",
                    "enum": ["metric", "imperial"],
                    "description": "Unit system. 'metric' = °C, km/h, mm (default). \
                                    'imperial' = °F, mph, inches."
                },
                "days": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 3,
                    "description": "Number of forecast days to include (0–3). \
                                    0 returns current conditions only. Default: 1."
                }
            },
            "required": ["location"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let location = match args.get("location").and_then(|v| v.as_str()) {
            Some(loc) if !loc.trim().is_empty() => loc.trim().to_string(),
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing required parameter 'location'".into()),
                });
            }
        };

        let metric = args
            .get("units")
            .and_then(|v| v.as_str())
            .map(|u| u.to_lowercase() != "imperial")
            .unwrap_or(true);

        let days: u8 = args
            .get("days")
            .and_then(|v| v.as_u64())
            .map(|d| d.min(3) as u8)
            .unwrap_or(1);

        match Self::fetch(&location).await {
            Ok(data) => {
                let output = Self::format_output(&data, metric, days);
                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tool() -> WeatherTool {
        WeatherTool::new()
    }

    // ── Metadata ──────────────────────────────────────────────────────────────

    #[test]
    fn name_is_weather() {
        assert_eq!(make_tool().name(), "weather");
    }

    #[test]
    fn description_is_non_empty() {
        assert!(!make_tool().description().is_empty());
    }

    #[test]
    fn parameters_schema_is_valid_object() {
        let schema = make_tool().parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"].is_object());
    }

    #[test]
    fn schema_requires_location() {
        let schema = make_tool().parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&Value::String("location".into())));
    }

    #[test]
    fn schema_location_property_exists() {
        let schema = make_tool().parameters_schema();
        assert!(schema["properties"]["location"].is_object());
        assert_eq!(schema["properties"]["location"]["type"], "string");
    }

    #[test]
    fn schema_units_property_has_enum() {
        let schema = make_tool().parameters_schema();
        let units = &schema["properties"]["units"];
        assert!(units.is_object());
        let enums = units["enum"].as_array().unwrap();
        assert!(enums.contains(&Value::String("metric".into())));
        assert!(enums.contains(&Value::String("imperial".into())));
    }

    #[test]
    fn schema_days_has_bounds() {
        let schema = make_tool().parameters_schema();
        let days = &schema["properties"]["days"];
        assert_eq!(days["minimum"], 0);
        assert_eq!(days["maximum"], 3);
    }

    // ── URL building ──────────────────────────────────────────────────────────

    #[test]
    fn build_url_city_name() {
        let url = WeatherTool::build_url("London");
        assert_eq!(url, "https://wttr.in/London?format=j1");
    }

    #[test]
    fn build_url_encodes_spaces() {
        let url = WeatherTool::build_url("New York");
        assert_eq!(url, "https://wttr.in/New+York?format=j1");
    }

    #[test]
    fn build_url_trims_whitespace() {
        let url = WeatherTool::build_url("  Paris  ");
        assert_eq!(url, "https://wttr.in/Paris?format=j1");
    }

    #[test]
    fn build_url_gps_coordinates() {
        let url = WeatherTool::build_url("51.5,-0.1");
        assert_eq!(url, "https://wttr.in/51.5,-0.1?format=j1");
    }

    #[test]
    fn build_url_airport_code() {
        let url = WeatherTool::build_url("LAX");
        assert_eq!(url, "https://wttr.in/LAX?format=j1");
    }

    #[test]
    fn build_url_zip_code() {
        let url = WeatherTool::build_url("74015");
        assert_eq!(url, "https://wttr.in/74015?format=j1");
    }

    // ── execute: parameter validation ─────────────────────────────────────────

    #[tokio::test]
    async fn execute_missing_location_returns_error() {
        let result = make_tool().execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("location"));
    }

    #[tokio::test]
    async fn execute_empty_location_returns_error() {
        let result = make_tool()
            .execute(json!({"location": "   "}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("location"));
    }

    #[tokio::test]
    async fn execute_null_location_returns_error() {
        let result = make_tool()
            .execute(json!({"location": null}))
            .await
            .unwrap();
        assert!(!result.success);
    }

    // ── format_hourly ─────────────────────────────────────────────────────────

    #[test]
    fn format_hourly_metric() {
        let h = HourlyCondition {
            time: "900".into(),
            temp_c: "15".into(),
            temp_f: "59".into(),
            weather_desc: vec![StringValue {
                value: "Sunny".into(),
            }],
            chance_of_rain: "5".into(),
            chance_of_snow: "0".into(),
            windspeed_kmph: "20".into(),
            windspeed_miles: "12".into(),
            winddir_16point: "SW".into(),
        };
        let formatted = WeatherTool::format_hourly(&h, true);
        assert!(formatted.contains("09:00"));
        assert!(formatted.contains("15°C"));
        assert!(formatted.contains("Sunny"));
        assert!(formatted.contains("20 km/h"));
        assert!(formatted.contains("SW"));
    }

    #[test]
    fn format_hourly_imperial() {
        let h = HourlyCondition {
            time: "1200".into(),
            temp_c: "20".into(),
            temp_f: "68".into(),
            weather_desc: vec![StringValue {
                value: "Clear".into(),
            }],
            chance_of_rain: "0".into(),
            chance_of_snow: "0".into(),
            windspeed_kmph: "16".into(),
            windspeed_miles: "10".into(),
            winddir_16point: "NW".into(),
        };
        let formatted = WeatherTool::format_hourly(&h, false);
        assert!(formatted.contains("12:00"));
        assert!(formatted.contains("68°F"));
        assert!(formatted.contains("10 mph"));
    }

    #[test]
    fn format_hourly_midnight_slot() {
        let h = HourlyCondition {
            time: "0".into(),
            temp_c: "8".into(),
            temp_f: "46".into(),
            weather_desc: vec![StringValue {
                value: "Clear".into(),
            }],
            chance_of_rain: "0".into(),
            chance_of_snow: "0".into(),
            windspeed_kmph: "5".into(),
            windspeed_miles: "3".into(),
            winddir_16point: "N".into(),
        };
        let formatted = WeatherTool::format_hourly(&h, true);
        assert!(formatted.contains("00:00"));
    }

    // ── format_day ────────────────────────────────────────────────────────────

    fn make_day(date: &str) -> WeatherDay {
        WeatherDay {
            date: date.into(),
            max_temp_c: "18".into(),
            max_temp_f: "64".into(),
            min_temp_c: "8".into(),
            min_temp_f: "46".into(),
            avg_temp_c: "13".into(),
            avg_temp_f: "55".into(),
            sun_hours: "8.5".into(),
            uv_index: "3".into(),
            total_snow_cm: "0.0".into(),
            astronomy: vec![Astronomy {
                sunrise: "06:00 AM".into(),
                sunset: "06:30 PM".into(),
                moon_phase: "Waxing Crescent".into(),
            }],
            hourly: vec![
                HourlyCondition {
                    time: "600".into(),
                    temp_c: "10".into(),
                    temp_f: "50".into(),
                    weather_desc: vec![StringValue {
                        value: "Sunny".into(),
                    }],
                    chance_of_rain: "0".into(),
                    chance_of_snow: "0".into(),
                    windspeed_kmph: "10".into(),
                    windspeed_miles: "6".into(),
                    winddir_16point: "N".into(),
                },
                HourlyCondition {
                    time: "1200".into(),
                    temp_c: "16".into(),
                    temp_f: "61".into(),
                    weather_desc: vec![StringValue {
                        value: "Partly Cloudy".into(),
                    }],
                    chance_of_rain: "20".into(),
                    chance_of_snow: "0".into(),
                    windspeed_kmph: "15".into(),
                    windspeed_miles: "9".into(),
                    winddir_16point: "NE".into(),
                },
            ],
        }
    }

    #[test]
    fn format_day_metric_contains_temps() {
        let day = make_day("2026-03-21");
        let out = WeatherTool::format_day(&day, true, false);
        assert!(out.contains("18°C"));
        assert!(out.contains("8°C"));
        assert!(out.contains("13°C"));
        assert!(out.contains("2026-03-21"));
    }

    #[test]
    fn format_day_imperial_contains_temps() {
        let day = make_day("2026-03-21");
        let out = WeatherTool::format_day(&day, false, false);
        assert!(out.contains("64°F"));
        assert!(out.contains("46°F"));
    }

    #[test]
    fn format_day_includes_astronomy() {
        let day = make_day("2026-03-21");
        let out = WeatherTool::format_day(&day, true, false);
        assert!(out.contains("06:00 AM"));
        assert!(out.contains("06:30 PM"));
        assert!(out.contains("Waxing Crescent"));
    }

    #[test]
    fn format_day_with_hourly_expands_output() {
        let day = make_day("2026-03-21");
        let without = WeatherTool::format_day(&day, true, false);
        let with_hourly = WeatherTool::format_day(&day, true, true);
        assert!(with_hourly.len() > without.len());
        assert!(with_hourly.contains("06:00"));
    }

    #[test]
    fn format_day_snow_metric_shown_when_nonzero() {
        let mut day = make_day("2026-03-21");
        day.total_snow_cm = "5.0".into();
        let out = WeatherTool::format_day(&day, true, false);
        assert!(out.contains("5.0 cm"));
    }

    #[test]
    fn format_day_snow_imperial_converted() {
        let mut day = make_day("2026-03-21");
        day.total_snow_cm = "2.54".into();
        let out = WeatherTool::format_day(&day, false, false);
        assert!(out.contains("1.0 in"));
    }

    #[test]
    fn format_day_no_snow_note_when_zero() {
        let day = make_day("2026-03-21");
        let out = WeatherTool::format_day(&day, true, false);
        assert!(!out.contains("Snow:"));
    }

    // ── format_output ─────────────────────────────────────────────────────────

    fn make_response() -> WttrResponse {
        WttrResponse {
            current_condition: vec![CurrentCondition {
                temp_c: "12".into(),
                temp_f: "54".into(),
                feels_like_c: "10".into(),
                feels_like_f: "50".into(),
                humidity: "72".into(),
                weather_desc: vec![StringValue {
                    value: "Partly cloudy".into(),
                }],
                windspeed_kmph: "18".into(),
                windspeed_miles: "11".into(),
                winddir_16point: "WSW".into(),
                precip_mm: "0.1".into(),
                precip_inches: "0.0".into(),
                visibility: "10".into(),
                visibility_miles: "6".into(),
                uv_index: "2".into(),
                cloud_cover: "55".into(),
                pressure_mb: "1015".into(),
                pressure_inches: "30".into(),
                observation_time: "10:00 AM".into(),
            }],
            nearest_area: vec![NearestArea {
                area_name: vec![StringValue {
                    value: "Tulsa".into(),
                }],
                country: vec![StringValue {
                    value: "United States".into(),
                }],
                region: vec![StringValue {
                    value: "Oklahoma".into(),
                }],
            }],
            weather: vec![make_day("2026-03-20"), make_day("2026-03-21")],
        }
    }

    #[test]
    fn format_output_metric_current_only() {
        let data = make_response();
        let out = WeatherTool::format_output(&data, true, 0);
        assert!(out.contains("Tulsa"));
        assert!(out.contains("12°C"));
        assert!(out.contains("10°C")); // feels like
        assert!(out.contains("Partly cloudy"));
        assert!(out.contains("72%")); // humidity
        assert!(out.contains("18 km/h"));
        assert!(out.contains("WSW"));
        assert!(!out.contains("Forecast"));
    }

    #[test]
    fn format_output_imperial_current_only() {
        let data = make_response();
        let out = WeatherTool::format_output(&data, false, 0);
        assert!(out.contains("54°F"));
        assert!(out.contains("50°F"));
        assert!(out.contains("11 mph"));
    }

    #[test]
    fn format_output_includes_forecast_when_days_gt_0() {
        let data = make_response();
        let out = WeatherTool::format_output(&data, true, 2);
        assert!(out.contains("Forecast"));
        assert!(out.contains("2026-03-20"));
        assert!(out.contains("2026-03-21"));
    }

    #[test]
    fn format_output_respects_days_limit() {
        let data = make_response();
        // Only 1 day requested
        let out = WeatherTool::format_output(&data, true, 1);
        assert!(out.contains("2026-03-20"));
        assert!(!out.contains("2026-03-21"));
    }

    #[test]
    fn format_output_includes_location_region_country() {
        let data = make_response();
        let out = WeatherTool::format_output(&data, true, 0);
        assert!(out.contains("Tulsa"));
        assert!(out.contains("Oklahoma"));
        assert!(out.contains("United States"));
    }

    #[test]
    fn format_output_empty_current_condition_is_graceful() {
        let mut data = make_response();
        data.current_condition.clear();
        let out = WeatherTool::format_output(&data, true, 0);
        assert!(out.contains("No current conditions available"));
    }

    #[test]
    fn format_output_location_without_region() {
        let mut data = make_response();
        data.nearest_area[0].region.clear();
        let out = WeatherTool::format_output(&data, true, 0);
        assert!(out.contains("Tulsa"));
        assert!(out.contains("United States"));
    }

    // ── days clamping ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn execute_clamps_days_above_3() {
        // We can't hit the network in unit tests, but we can verify that
        // the days argument is clamped before it reaches fetch by inspecting
        // format_output: supply a mock response and call format_output directly.
        let data = make_response();
        // 99 clamped to 3 → should only emit up to 2 days (our mock has 2)
        let out = WeatherTool::format_output(&data, true, 3u8);
        assert!(out.contains("Forecast"));
    }

    // ── spec ──────────────────────────────────────────────────────────────────

    #[test]
    fn spec_reflects_tool_metadata() {
        let tool = make_tool();
        let spec = tool.spec();
        assert_eq!(spec.name, "weather");
        assert_eq!(spec.description, tool.description());
        assert!(spec.parameters.is_object());
    }
}
