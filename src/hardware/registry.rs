//! Board registry — maps USB VID/PID to known board names and architectures.

/// Information about a known board.
#[derive(Debug, Clone)]
pub struct BoardInfo {
    pub vid: u16,
    pub pid: u16,
    pub name: &'static str,
    pub architecture: Option<&'static str>,
}

/// Known USB VID/PID to board mappings.
/// VID 0x0483 = STMicroelectronics, 0x2341 = Arduino, 0x10c4 = Silicon Labs.
const KNOWN_BOARDS: &[BoardInfo] = &[
    BoardInfo {
        vid: 0x0483,
        pid: 0x374b,
        name: "nucleo-f401re",
        architecture: Some("ARM Cortex-M4"),
    },
    BoardInfo {
        vid: 0x0483,
        pid: 0x3748,
        name: "nucleo-f411re",
        architecture: Some("ARM Cortex-M4"),
    },
    BoardInfo {
        vid: 0x2341,
        pid: 0x0043,
        name: "arduino-uno",
        architecture: Some("AVR ATmega328P"),
    },
    BoardInfo {
        vid: 0x2341,
        pid: 0x0078,
        name: "arduino-uno",
        architecture: Some("Arduino Uno Q / ATmega328P"),
    },
    BoardInfo {
        vid: 0x2341,
        pid: 0x0042,
        name: "arduino-mega",
        architecture: Some("AVR ATmega2560"),
    },
    BoardInfo {
        vid: 0x10c4,
        pid: 0xea60,
        name: "cp2102",
        architecture: Some("USB-UART bridge"),
    },
    BoardInfo {
        vid: 0x10c4,
        pid: 0xea70,
        name: "cp2102n",
        architecture: Some("USB-UART bridge"),
    },
    // ESP32 dev boards often use CH340 USB-UART
    BoardInfo {
        vid: 0x1a86,
        pid: 0x7523,
        name: "esp32",
        architecture: Some("ESP32 (CH340)"),
    },
    BoardInfo {
        vid: 0x1a86,
        pid: 0x55d4,
        name: "esp32",
        architecture: Some("ESP32 (CH340)"),
    },
    // Raspberry Pi Pico (VID 0x2E8A = Raspberry Pi Foundation)
    BoardInfo {
        vid: 0x2e8a,
        pid: 0x000a,
        name: "raspberry-pi-pico",
        architecture: Some("ARM Cortex-M0+ (RP2040)"),
    },
    BoardInfo {
        vid: 0x2e8a,
        pid: 0x0005,
        name: "raspberry-pi-pico",
        architecture: Some("ARM Cortex-M0+ (RP2040)"),
    },
    // Pico W (with CYW43 wireless)
    // NOTE: PID 0xF00A is not in the official Raspberry Pi USB PID allocation.
    // MicroPython on Pico W typically uses PID 0x0005 (CDC REPL). This entry
    // is a placeholder for custom ZeroClaw firmware that sets PID 0xF00A.
    // If using stock MicroPython, the Pico W will match the 0x0005 entry above.
    // Reference: https://github.com/raspberrypi/usb-pid (official PID list).
    BoardInfo {
        vid: 0x2e8a,
        pid: 0xf00a,
        name: "raspberry-pi-pico-w",
        architecture: Some("ARM Cortex-M0+ (RP2040 + CYW43)"),
    },
];

/// Look up a board by VID and PID.
pub fn lookup_board(vid: u16, pid: u16) -> Option<&'static BoardInfo> {
    KNOWN_BOARDS.iter().find(|b| b.vid == vid && b.pid == pid)
}

/// Return all known board entries.
pub fn known_boards() -> &'static [BoardInfo] {
    KNOWN_BOARDS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_nucleo_f401re() {
        let b = lookup_board(0x0483, 0x374b).unwrap();
        assert_eq!(b.name, "nucleo-f401re");
        assert_eq!(b.architecture, Some("ARM Cortex-M4"));
    }

    #[test]
    fn lookup_unknown_returns_none() {
        assert!(lookup_board(0x0000, 0x0000).is_none());
    }

    #[test]
    fn known_boards_not_empty() {
        assert!(!known_boards().is_empty());
    }

    #[test]
    fn lookup_pico_standard() {
        let b = lookup_board(0x2e8a, 0x000a).unwrap();
        assert_eq!(b.name, "raspberry-pi-pico");
        assert!(b.architecture.unwrap().contains("RP2040"));
    }

    #[test]
    fn lookup_pico_w() {
        let b = lookup_board(0x2e8a, 0xf00a).unwrap();
        assert_eq!(b.name, "raspberry-pi-pico-w");
        assert!(b.architecture.unwrap().contains("CYW43"));
    }
}
