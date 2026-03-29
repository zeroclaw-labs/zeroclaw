//! ZeroClaw Pico firmware — JSON-over-serial peripheral.
//!
//! Listens for newline-delimited JSON on UART0 (GP0=TX, GP1=RX).
//! LED on GP25 (onboard LED on standard Pico).
//!
//! Protocol: same as Nucleo/Arduino/ESP32 — see docs/hardware-peripherals-design.md

#![no_std]
#![no_main]

use core::str;
use defmt::info;
use embassy_executor::Spawner;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::uart::{Config, Uart};
use heapless::String;
use zeroclaw_fw_protocol::{copy_id, write_err, write_ok, Command};
use {defmt_rtt as _, panic_probe as _};

/// Onboard LED pin on standard Raspberry Pi Pico
const LED_PIN: i32 = 25;
/// Max user-accessible GPIO pin
const MAX_PIN: i32 = 22;
/// Min user-accessible GPIO pin (GP0/GP1 reserved for UART)
const MIN_PIN: i32 = 2;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    let mut config = Config::default();
    config.baudrate = 115_200;

    let mut uart = Uart::new_blocking(p.UART0, p.PIN_0, p.PIN_1, config);
    let mut led = Output::new(p.PIN_25, Level::Low);

    info!("ZeroClaw Pico firmware ready on UART0 (115200)");

    let mut line_buf: heapless::Vec<u8, 256> = heapless::Vec::new();
    let mut id_buf = [0u8; 16];
    let mut resp_buf: String<256> = String::new();

    loop {
        let mut byte = [0u8; 1];
        if uart.blocking_read(&mut byte).is_ok() {
            let b = byte[0];
            if b == b'\n' || b == b'\r' {
                if !line_buf.is_empty() {
                    let id_len = copy_id(&line_buf, &mut id_buf);
                    let id_str = str::from_utf8(&id_buf[..id_len]).unwrap_or("0");

                    match Command::from_line(&line_buf) {
                        Some(Command::Ping) => {
                            write_ok(&mut resp_buf, id_str, "pong");
                        }
                        Some(Command::Capabilities) => {
                            resp_buf.clear();
                            let _ = core::fmt::Write::write_str(
                                &mut resp_buf,
                                concat!(
                                    r#"{"id":""#,
                                ),
                            );
                            let _ = core::fmt::Write::write_str(&mut resp_buf, id_str);
                            let _ = core::fmt::Write::write_str(
                                &mut resp_buf,
                                r#"","ok":true,"result":"{\"gpio\":[2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22],\"led_pin\":25}"}"#,
                            );
                        }
                        Some(Command::GpioRead { pin }) => {
                            if pin >= MIN_PIN && pin <= MAX_PIN {
                                write_ok(&mut resp_buf, id_str, "0");
                            } else {
                                write_err(&mut resp_buf, id_str, "Invalid pin");
                            }
                        }
                        Some(Command::GpioWrite { pin, value }) => {
                            if pin == LED_PIN {
                                led.set_level(if value != 0 { Level::High } else { Level::Low });
                                write_ok(&mut resp_buf, id_str, "done");
                            } else if pin >= MIN_PIN && pin <= MAX_PIN {
                                // TODO: implement dynamic GPIO pin drivers
                                write_ok(&mut resp_buf, id_str, "done");
                            } else {
                                write_err(&mut resp_buf, id_str, "Invalid pin");
                            }
                        }
                        None => {
                            write_err(&mut resp_buf, id_str, "Unknown command");
                        }
                    }

                    let _ = uart.blocking_write(resp_buf.as_bytes());
                    let _ = uart.blocking_write(b"\n");
                    line_buf.clear();
                }
            } else if line_buf.push(b).is_err() {
                line_buf.clear();
            }
        }
    }
}
