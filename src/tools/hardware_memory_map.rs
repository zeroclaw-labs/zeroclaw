//! Hardware memory map tool — returns flash/RAM address ranges for connected boards.
//!
//! Phase B: When user asks "what are the upper and lower memory addresses?", this tool
//! returns the memory map. Uses probe-rs for Nucleo/STM32 when available; otherwise
//! returns static maps from datasheets.

use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::fmt::Write as _;

/// Known memory maps (from datasheets). Used when probe-rs is unavailable.
const MEMORY_MAPS: &[(&str, &str)] = &[
    (
        "nucleo-f401re",
        "Flash: 0x0800_0000 - 0x0807_FFFF (512 KB)\nRAM: 0x2000_0000 - 0x2001_FFFF (128 KB)\nSTM32F401RET6, ARM Cortex-M4",
    ),
    (
        "nucleo-f411re",
        "Flash: 0x0800_0000 - 0x0807_FFFF (512 KB)\nRAM: 0x2000_0000 - 0x2001_FFFF (128 KB)\nSTM32F411RET6, ARM Cortex-M4",
    ),
    (
        "arduino-uno",
        "Flash: 0x0000 - 0x3FFF (16 KB, ATmega328P)\nSRAM: 0x0100 - 0x08FF (2 KB)\nEEPROM: 0x0000 - 0x03FF (1 KB)",
    ),
    (
        "arduino-mega",
        "Flash: 0x0000 - 0x3FFFF (256 KB, ATmega2560)\nSRAM: 0x0200 - 0x21FF (8 KB)\nEEPROM: 0x0000 - 0x0FFF (4 KB)",
    ),
    (
        "esp32",
        "Flash: 0x3F40_0000 - 0x3F7F_FFFF (4 MB typical)\nIRAM: 0x4000_0000 - 0x4005_FFFF\nDRAM: 0x3FFB_0000 - 0x3FFF_FFFF",
    ),
];

/// Tool: report hardware memory map for connected boards.
pub struct HardwareMemoryMapTool {
    boards: Vec<String>,
}

impl HardwareMemoryMapTool {
    pub fn new(boards: Vec<String>) -> Self {
        Self { boards }
    }

    fn static_map_for_board(&self, board: &str) -> Option<&'static str> {
        MEMORY_MAPS
            .iter()
            .find(|(b, _)| *b == board)
            .map(|(_, m)| *m)
    }
}

#[async_trait]
impl Tool for HardwareMemoryMapTool {
    fn name(&self) -> &str {
        "hardware_memory_map"
    }

    fn description(&self) -> &str {
        "Return the memory map (flash and RAM address ranges) for connected hardware. Use when: user asks for 'upper and lower memory addresses', 'memory map', 'address space', or 'readable addresses'. Returns flash/RAM ranges from datasheets."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "board": {
                    "type": "string",
                    "description": "Optional board name (e.g. nucleo-f401re, arduino-uno). If omitted, returns map for first configured board."
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let board = args
            .get("board")
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| self.boards.first().cloned());

        let board = board.as_deref().unwrap_or("unknown");

        if self.boards.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "No peripherals configured. Add boards to config.toml [peripherals.boards]."
                        .into(),
                ),
            });
        }

        let mut output = String::new();

        #[cfg(feature = "probe")]
        let probe_ok = {
            if board == "nucleo-f401re" || board == "nucleo-f411re" {
                let chip = if board == "nucleo-f411re" {
                    "STM32F411RETx"
                } else {
                    "STM32F401RETx"
                };
                match probe_rs_memory_map(chip) {
                    Ok(probe_msg) => {
                        let _ = write!(output, "**{board}** (via probe-rs):\n{probe_msg}\n");
                        true
                    }
                    Err(e) => {
                        let _ = write!(output, "Probe-rs failed: {e}. ");
                        false
                    }
                }
            } else {
                false
            }
        };

        #[cfg(not(feature = "probe"))]
        let probe_ok = false;

        if !probe_ok {
            if let Some(map) = self.static_map_for_board(board) {
                use std::fmt::Write;
                let _ = write!(output, "**{board}** (from datasheet):\n{map}");
            } else {
                use std::fmt::Write;
                let known: Vec<&str> = MEMORY_MAPS.iter().map(|(b, _)| *b).collect();
                let _ = write!(
                    output,
                    "No memory map for board '{board}'. Known boards: {}",
                    known.join(", ")
                );
            }
        }

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

#[cfg(feature = "probe")]
fn probe_rs_memory_map(chip: &str) -> anyhow::Result<String> {
    use probe_rs::config::MemoryRegion;
    use probe_rs::{Session, SessionConfig};

    let session = Session::auto_attach(chip, SessionConfig::default())
        .map_err(|e| anyhow::anyhow!("probe-rs attach failed: {}", e))?;

    let target = session.target();
    let mut out = String::new();

    for region in target.memory_map.iter() {
        match region {
            MemoryRegion::Ram(ram) => {
                let start = ram.range.start;
                let end = ram.range.end;
                append_region_line(&mut out, "RAM", start, end);
            }
            MemoryRegion::Nvm(flash) => {
                let start = flash.range.start;
                let end = flash.range.end;
                append_region_line(&mut out, "Flash", start, end);
            }
            _ => {}
        }
    }

    if out.is_empty() {
        out = "Could not read memory regions from probe.".to_string();
    }

    Ok(out)
}

fn append_region_line(out: &mut String, label: &str, start: u64, end: u64) {
    let size_kb = end.saturating_sub(start) / 1024;
    let _ = writeln!(out, "{label}: 0x{start:08X} - 0x{end:08X} ({size_kb} KB)");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_map_nucleo() {
        let tool = HardwareMemoryMapTool::new(vec!["nucleo-f401re".into()]);
        assert!(tool.static_map_for_board("nucleo-f401re").is_some());
        assert!(tool
            .static_map_for_board("nucleo-f401re")
            .unwrap()
            .contains("Flash"));
    }

    #[test]
    fn static_map_arduino() {
        let tool = HardwareMemoryMapTool::new(vec!["arduino-uno".into()]);
        assert!(tool.static_map_for_board("arduino-uno").is_some());
    }

    #[test]
    fn append_region_line_matches_legacy_format() {
        let mut actual = String::new();
        append_region_line(&mut actual, "RAM", 0x2000_0000, 0x2002_0000);
        let expected = format!(
            "RAM: 0x{:08X} - 0x{:08X} ({} KB)\n",
            0x2000_0000_u64,
            0x2002_0000_u64,
            (0x2002_0000_u64 - 0x2000_0000_u64) / 1024
        );
        assert_eq!(actual, expected);
    }
}
