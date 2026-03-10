//! ZeroClaw Nucleo-F401RE firmware — JSON-over-serial peripheral.
//!
//! Listens for newline-delimited JSON on USART2 (PA2=TX, PA3=RX).
//! USART2 is connected to ST-Link VCP — host sees /dev/ttyACM0 (Linux) or /dev/cu.usbmodem* (macOS).
//!
//! Protocol: same as Arduino/ESP32 — see docs/hardware-peripherals-design.md

#![no_std]
#![no_main]

use core::fmt::Write;
use core::str;
use defmt::info;
use embassy_executor::Spawner;
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_stm32::usart::{Config, Uart};
use heapless::String;
use {defmt_rtt as _, panic_probe as _};

/// Arduino-style pin 13 = PA5 (User LED LD2 on Nucleo-F401RE)
const LED_PIN: u8 = 13;

/// Parse integer from JSON: "pin":13 or "value":1
fn parse_arg(line: &[u8], key: &[u8]) -> Option<i32> {
    // key like b"pin" -> search for b"\"pin\":"
    let mut suffix: [u8; 32] = [0; 32];
    suffix[0] = b'"';
    let mut len = 1;
    for (i, &k) in key.iter().enumerate() {
        if i >= 30 {
            break;
        }
        suffix[len] = k;
        len += 1;
    }
    suffix[len] = b'"';
    suffix[len + 1] = b':';
    len += 2;
    let suffix = &suffix[..len];

    let line_len = line.len();
    if line_len < len {
        return None;
    }
    for i in 0..=line_len - len {
        if line[i..].starts_with(suffix) {
            let rest = &line[i + len..];
            let mut num: i32 = 0;
            let mut neg = false;
            let mut j = 0;
            if j < rest.len() && rest[j] == b'-' {
                neg = true;
                j += 1;
            }
            while j < rest.len() && rest[j].is_ascii_digit() {
                num = num * 10 + (rest[j] - b'0') as i32;
                j += 1;
            }
            return Some(if neg { -num } else { num });
        }
    }
    None
}

fn has_cmd(line: &[u8], cmd: &[u8]) -> bool {
    let mut pat: [u8; 64] = [0; 64];
    pat[0..7].copy_from_slice(b"\"cmd\":\"");
    let clen = cmd.len().min(50);
    pat[7..7 + clen].copy_from_slice(&cmd[..clen]);
    pat[7 + clen] = b'"';
    let pat = &pat[..8 + clen];

    let line_len = line.len();
    if line_len < pat.len() {
        return false;
    }
    for i in 0..=line_len - pat.len() {
        if line[i..].starts_with(pat) {
            return true;
        }
    }
    false
}

/// Extract "id" for response
fn copy_id(line: &[u8], out: &mut [u8]) -> usize {
    let prefix = b"\"id\":\"";
    if line.len() < prefix.len() + 1 {
        out[0] = b'0';
        return 1;
    }
    for i in 0..=line.len() - prefix.len() {
        if line[i..].starts_with(prefix) {
            let start = i + prefix.len();
            let mut j = 0;
            while start + j < line.len() && j < out.len() - 1 && line[start + j] != b'"' {
                out[j] = line[start + j];
                j += 1;
            }
            return j;
        }
    }
    out[0] = b'0';
    1
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_stm32::init(Default::default());

    let mut config = Config::default();
    config.baudrate = 115_200;

    let mut usart = Uart::new_blocking(p.USART2, p.PA3, p.PA2, config).unwrap();
    let mut led = Output::new(p.PA5, Level::Low, Speed::Low);

    info!("ZeroClaw Nucleo firmware ready on USART2 (115200)");

    let mut line_buf: heapless::Vec<u8, 256> = heapless::Vec::new();
    let mut id_buf = [0u8; 16];
    let mut resp_buf: String<128> = String::new();

    loop {
        let mut byte = [0u8; 1];
        if usart.blocking_read(&mut byte).is_ok() {
            let b = byte[0];
            if b == b'\n' || b == b'\r' {
                if !line_buf.is_empty() {
                    let id_len = copy_id(&line_buf, &mut id_buf);
                    let id_str = str::from_utf8(&id_buf[..id_len]).unwrap_or("0");

                    resp_buf.clear();
                    if has_cmd(&line_buf, b"ping") {
                        let _ = write!(resp_buf, "{{\"id\":\"{}\",\"ok\":true,\"result\":\"pong\"}}", id_str);
                    } else if has_cmd(&line_buf, b"capabilities") {
                        let _ = write!(
                            resp_buf,
                            "{{\"id\":\"{}\",\"ok\":true,\"result\":\"{{\\\"gpio\\\":[0,1,2,3,4,5,6,7,8,9,10,11,12,13],\\\"led_pin\\\":13}}\"}}",
                            id_str
                        );
                    } else if has_cmd(&line_buf, b"gpio_read") {
                        let pin = parse_arg(&line_buf, b"pin").unwrap_or(-1);
                        if pin == LED_PIN as i32 {
                            // Output doesn't support read; return 0 (LED state not readable)
                            let _ = write!(resp_buf, "{{\"id\":\"{}\",\"ok\":true,\"result\":\"0\"}}", id_str);
                        } else if pin >= 0 && pin <= 13 {
                            let _ = write!(resp_buf, "{{\"id\":\"{}\",\"ok\":true,\"result\":\"0\"}}", id_str);
                        } else {
                            let _ = write!(
                                resp_buf,
                                "{{\"id\":\"{}\",\"ok\":false,\"result\":\"\",\"error\":\"Invalid pin {}\"}}",
                                id_str, pin
                            );
                        }
                    } else if has_cmd(&line_buf, b"gpio_write") {
                        let pin = parse_arg(&line_buf, b"pin").unwrap_or(-1);
                        let value = parse_arg(&line_buf, b"value").unwrap_or(0);
                        if pin == LED_PIN as i32 {
                            led.set_level(if value != 0 { Level::High } else { Level::Low });
                            let _ = write!(resp_buf, "{{\"id\":\"{}\",\"ok\":true,\"result\":\"done\"}}", id_str);
                        } else if pin >= 0 && pin <= 13 {
                            let _ = write!(resp_buf, "{{\"id\":\"{}\",\"ok\":true,\"result\":\"done\"}}", id_str);
                        } else {
                            let _ = write!(
                                resp_buf,
                                "{{\"id\":\"{}\",\"ok\":false,\"result\":\"\",\"error\":\"Invalid pin {}\"}}",
                                id_str, pin
                            );
                        }
                    } else {
                        let _ = write!(
                            resp_buf,
                            "{{\"id\":\"{}\",\"ok\":false,\"result\":\"\",\"error\":\"Unknown command\"}}",
                            id_str
                        );
                    }

                    let _ = usart.blocking_write(resp_buf.as_bytes());
                    let _ = usart.blocking_write(b"\n");
                    line_buf.clear();
                }
            } else if line_buf.push(b).is_err() {
                line_buf.clear();
            }
        }
    }
}
