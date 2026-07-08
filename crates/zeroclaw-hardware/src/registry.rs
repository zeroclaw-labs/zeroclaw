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
    fn lookup_requires_both_vid_and_pid_to_match() {
        // A known VID with an unknown PID must not match (not a VID-only lookup).
        assert!(lookup_board(0x0483, 0xffff).is_none());
        // A known PID under the wrong VID must not match either.
        assert!(lookup_board(0x0000, 0x374b).is_none());
    }

    #[test]
    fn lookup_distinguishes_multiple_pids_for_same_board_name() {
        // arduino-uno is registered under two distinct PIDs; both resolve.
        assert_eq!(lookup_board(0x2341, 0x0043).unwrap().name, "arduino-uno");
        assert_eq!(lookup_board(0x2341, 0x0078).unwrap().name, "arduino-uno");
        // arduino-mega shares the Arduino VID but a different PID.
        assert_eq!(lookup_board(0x2341, 0x0042).unwrap().name, "arduino-mega");
    }

    #[test]
    fn known_boards_have_unique_vid_pid_pairs() {
        let mut seen = std::collections::HashSet::new();
        for b in known_boards() {
            assert!(
                seen.insert((b.vid, b.pid)),
                "duplicate (vid, pid) entry: {:#06x}:{:#06x} ({})",
                b.vid,
                b.pid,
                b.name
            );
        }
    }

    #[test]
    fn every_known_board_resolves_to_itself() {
        for b in known_boards() {
            let found = lookup_board(b.vid, b.pid).unwrap();
            assert_eq!(found.name, b.name);
            assert_eq!(found.architecture, b.architecture);
        }
    }
}
