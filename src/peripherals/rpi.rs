//! Raspberry Pi GPIO peripheral — native rppal access.
//!
//! Only compiled when `peripheral-rpi` feature is enabled and target is Linux.
//! Uses BCM pin numbering (e.g. GPIO 17, 27).

use std::sync::Arc;

use crate::config::PeripheralBoardConfig;
use crate::dispatch::{DispatchAuditLogger, EventRouter};
use crate::peripherals::signal::emit_signal;
use crate::peripherals::traits::Peripheral;
use crate::tools::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::{json, Value};

/// RPi GPIO peripheral — direct access via rppal.
pub struct RpiGpioPeripheral {
    board: PeripheralBoardConfig,
}

impl RpiGpioPeripheral {
    /// Create a new RPi GPIO peripheral from config.
    pub fn new(board: PeripheralBoardConfig) -> Self {
        Self { board }
    }

    /// Attempt to connect (init rppal). Returns Ok if GPIO is available.
    pub async fn connect_from_config(board: &PeripheralBoardConfig) -> anyhow::Result<Self> {
        let mut peripheral = Self::new(board.clone());
        peripheral.connect().await?;
        Ok(peripheral)
    }
}

#[async_trait]
impl Peripheral for RpiGpioPeripheral {
    fn name(&self) -> &str {
        &self.board.board
    }

    fn board_type(&self) -> &str {
        "rpi-gpio"
    }

    async fn connect(&mut self) -> anyhow::Result<()> {
        // Verify GPIO is accessible by doing a no-op init
        let result = tokio::task::spawn_blocking(|| rppal::gpio::Gpio::new()).await??;
        drop(result);
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn health_check(&self) -> bool {
        tokio::task::spawn_blocking(|| rppal::gpio::Gpio::new().is_ok())
            .await
            .unwrap_or(false)
    }

    fn tools(&self) -> Vec<Box<dyn Tool>> {
        vec![Box::new(RpiGpioReadTool), Box::new(RpiGpioWriteTool)]
    }
}

/// Tool: read GPIO pin value (BCM numbering).
struct RpiGpioReadTool;

#[async_trait]
impl Tool for RpiGpioReadTool {
    fn name(&self) -> &str {
        "gpio_read"
    }

    fn description(&self) -> &str {
        "Read the value (0 or 1) of a GPIO pin on Raspberry Pi. Uses BCM pin numbers (e.g. 17, 27)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pin": {
                    "type": "integer",
                    "description": "BCM GPIO pin number (e.g. 17, 27)"
                }
            },
            "required": ["pin"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let pin = args
            .get("pin")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Missing 'pin' parameter"))?;
        let pin_u8 = pin as u8;

        let value = tokio::task::spawn_blocking(move || {
            let gpio = rppal::gpio::Gpio::new()?;
            let pin = gpio.get(pin_u8)?.into_input();
            Ok::<_, anyhow::Error>(match pin.read() {
                rppal::gpio::Level::Low => 0,
                rppal::gpio::Level::High => 1,
            })
        })
        .await??;

        Ok(ToolResult {
            success: true,
            output: format!("pin {} = {}", pin, value),
            error: None,
        })
    }
}

/// Tool: write GPIO pin value (BCM numbering).
struct RpiGpioWriteTool;

#[async_trait]
impl Tool for RpiGpioWriteTool {
    fn name(&self) -> &str {
        "gpio_write"
    }

    fn description(&self) -> &str {
        "Set a GPIO pin high (1) or low (0) on Raspberry Pi. Uses BCM pin numbers."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pin": {
                    "type": "integer",
                    "description": "BCM GPIO pin number"
                },
                "value": {
                    "type": "integer",
                    "description": "0 for low, 1 for high"
                }
            },
            "required": ["pin", "value"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let pin = args
            .get("pin")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Missing 'pin' parameter"))?;
        let value = args
            .get("value")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Missing 'value' parameter"))?;
        let pin_u8 = pin as u8;
        let level = match value {
            0 => rppal::gpio::Level::Low,
            _ => rppal::gpio::Level::High,
        };

        tokio::task::spawn_blocking(move || {
            let gpio = rppal::gpio::Gpio::new()?;
            let mut pin = gpio.get(pin_u8)?.into_output();
            pin.write(level);
            Ok::<_, anyhow::Error>(())
        })
        .await??;

        Ok(ToolResult {
            success: true,
            output: format!("pin {} = {}", pin, value),
            error: None,
        })
    }
}

/// Handle returned by [`watch_pins`]. Dropping the handle releases the GPIO
/// interrupt registrations and stops the dispatch forwarding task.
pub struct GpioWatcher {
    /// Owned pins must outlive the interrupt callbacks.
    _pins: Vec<rppal::gpio::InputPin>,
    /// Cancel signal for the forwarding task.
    _cancel: tokio::sync::oneshot::Sender<()>,
}

/// Start watching a set of BCM GPIO pins and publish every level change as a
/// `Peripheral` dispatch event through `router` + `audit`.
///
/// The event topic is `{board}/pin_{n}` (e.g. `rpi-gpio/pin_17`) and the
/// payload is `"0"` or `"1"`. Both edges are reported.
///
/// rppal's `set_async_interrupt` callback runs on rppal's own polling thread,
/// so we forward each level change through an unbounded mpsc channel into a
/// tokio task that performs the (async) `emit_signal` call. This keeps the
/// interrupt latency tiny and avoids needing a tokio runtime handle inside
/// the rppal thread.
///
/// Returns a [`GpioWatcher`] handle. Dropping it stops the forwarding task
/// and releases the rppal pin handles (which clears the interrupts).
pub fn watch_pins(
    board: &str,
    pins: &[u8],
    router: Arc<EventRouter>,
    audit: Arc<DispatchAuditLogger>,
) -> anyhow::Result<GpioWatcher> {
    use rppal::gpio::{Gpio, Level, Trigger};

    let gpio = Gpio::new()?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(u8, Level)>();
    let mut owned_pins: Vec<rppal::gpio::InputPin> = Vec::with_capacity(pins.len());

    for &pin_num in pins {
        let mut input = gpio.get(pin_num)?.into_input_pullup();
        let tx_clone = tx.clone();
        // Both edges so we report falling and rising. Debouncing is the
        // application's responsibility (configure via dispatch handler logic).
        input
            .set_async_interrupt(Trigger::Both, move |level| {
                let _ = tx_clone.send((pin_num, level));
            })
            .map_err(|e| anyhow::anyhow!("failed to set interrupt on pin {pin_num}: {e}"))?;
        owned_pins.push(input);
    }
    drop(tx); // forwarding task uses cloned senders held by callbacks

    let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();
    let board_owned = board.to_string();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut cancel_rx => {
                    tracing::info!(board = %board_owned, "GPIO watcher: cancelled");
                    break;
                }
                maybe = rx.recv() => {
                    let Some((pin_num, level)) = maybe else {
                        tracing::info!(board = %board_owned, "GPIO watcher: channel closed");
                        break;
                    };
                    let payload = match level {
                        Level::High => "1",
                        Level::Low => "0",
                    };
                    let signal = format!("pin_{pin_num}");
                    if let Err(e) = emit_signal(&router, &audit, &board_owned, &signal, Some(payload)).await {
                        tracing::warn!(
                            board = %board_owned,
                            pin = pin_num,
                            "GPIO watcher: emit_signal failed: {e}"
                        );
                    }
                }
            }
        }
    });

    Ok(GpioWatcher {
        _pins: owned_pins,
        _cancel: cancel_tx,
    })
}
