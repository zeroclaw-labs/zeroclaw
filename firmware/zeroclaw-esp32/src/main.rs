//! ZeroClaw ESP32 firmware — JSON-over-serial peripheral.
//!
//! Listens for newline-delimited JSON commands on UART0, executes gpio_read/gpio_write,
//! responds with JSON. Compatible with host ZeroClaw SerialPeripheral protocol.
//!
//! Protocol: same as STM32 — see docs/hardware-peripherals-design.md

use esp_idf_svc::hal::gpio::PinDriver;
use esp_idf_svc::hal::prelude::*;
use esp_idf_svc::hal::uart::*;
use log::info;
use serde::{Deserialize, Serialize};

/// Incoming command from host.
#[derive(Debug, Deserialize)]
struct Request {
    id: String,
    cmd: String,
    args: serde_json::Value,
}

/// Outgoing response to host.
#[derive(Debug, Serialize)]
struct Response {
    id: String,
    ok: bool,
    result: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let peripherals = Peripherals::take()?;
    let pins = peripherals.pins;

    // UART0: TX=21, RX=20 (ESP32) — ESP32-C3 may use different pins; adjust for your board
    let config = UartConfig::new().baudrate(Hertz(115_200));
    let mut uart = UartDriver::new(
        peripherals.uart0,
        pins.gpio21,
        pins.gpio20,
        Option::<esp_idf_svc::hal::gpio::Gpio0>::None,
        Option::<esp_idf_svc::hal::gpio::Gpio1>::None,
        &config,
    )?;

    info!("ZeroClaw ESP32 firmware ready on UART0 (115200)");

    let mut buf = [0u8; 512];
    let mut line = Vec::new();

    loop {
        match uart.read(&mut buf, 100) {
            Ok(0) => continue,
            Ok(n) => {
                for &b in &buf[..n] {
                    if b == b'\n' {
                        if !line.is_empty() {
                            if let Ok(line_str) = std::str::from_utf8(&line) {
                                if let Ok(resp) = handle_request(line_str, &peripherals) {
                                    let out = serde_json::to_string(&resp).unwrap_or_default();
                                    let _ = uart.write(format!("{}\n", out).as_bytes());
                                }
                            }
                            line.clear();
                        }
                    } else {
                        line.push(b);
                        if line.len() > 400 {
                            line.clear();
                        }
                    }
                }
            }
            Err(_) => {}
        }
    }
}

fn handle_request(
    line: &str,
    peripherals: &esp_idf_svc::hal::peripherals::Peripherals,
) -> anyhow::Result<Response> {
    let req: Request = serde_json::from_str(line.trim())?;
    let id = req.id.clone();

    let result = match req.cmd.as_str() {
        "capabilities" => {
            // Phase C: report GPIO pins and LED pin (matches Arduino protocol)
            let caps = serde_json::json!({
                "gpio": [0, 1, 2, 3, 4, 5, 12, 13, 14, 15, 16, 17, 18, 19],
                "led_pin": 2
            });
            Ok(caps.to_string())
        }
        "gpio_read" => {
            let pin_num = req.args.get("pin").and_then(|v| v.as_u64()).unwrap_or(0) as i32;
            let value = gpio_read(peripherals, pin_num)?;
            Ok(value.to_string())
        }
        "gpio_write" => {
            let pin_num = req.args.get("pin").and_then(|v| v.as_u64()).unwrap_or(0) as i32;
            let value = req.args.get("value").and_then(|v| v.as_u64()).unwrap_or(0);
            gpio_write(peripherals, pin_num, value)?;
            Ok("done".into())
        }
        _ => Err(anyhow::anyhow!("Unknown command: {}", req.cmd)),
    };

    match result {
        Ok(r) => Ok(Response {
            id,
            ok: true,
            result: r,
            error: None,
        }),
        Err(e) => Ok(Response {
            id,
            ok: false,
            result: String::new(),
            error: Some(e.to_string()),
        }),
    }
}

fn gpio_read(_peripherals: &esp_idf_svc::hal::peripherals::Peripherals, _pin: i32) -> anyhow::Result<u8> {
    // TODO: implement input pin read — requires storing InputPin drivers per pin
    Ok(0)
}

fn gpio_write(
    peripherals: &esp_idf_svc::hal::peripherals::Peripherals,
    pin: i32,
    value: u64,
) -> anyhow::Result<()> {
    let pins = peripherals.pins;
    let level = value != 0;

    match pin {
        2 => {
            let mut out = PinDriver::output(pins.gpio2)?;
            out.set_level(esp_idf_svc::hal::gpio::Level::from(level))?;
        }
        13 => {
            let mut out = PinDriver::output(pins.gpio13)?;
            out.set_level(esp_idf_svc::hal::gpio::Level::from(level))?;
        }
        _ => anyhow::bail!("Pin {} not configured (add to gpio_write)", pin),
    }
    Ok(())
}
