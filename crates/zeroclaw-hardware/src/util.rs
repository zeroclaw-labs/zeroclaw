const SERIAL_ALLOWED_PATH_PREFIXES: &[&str] = &[
    "/dev/ttyACM",
    "/dev/ttyUSB",
    "/dev/tty.usbmodem",
    "/dev/cu.usbmodem",
    "/dev/tty.usbserial",
    "/dev/cu.usbserial",
    "COM",
    #[cfg(feature = "dev-sim")]
    DEV_SIM_SERIAL_PATH_PREFIX,
];

#[cfg(feature = "dev-sim")]
const DEV_SIM_SERIAL_PATH_PREFIX: &str = "/tmp/zc-sim-";

pub fn is_serial_path_allowed(path: &str) -> bool {
    SERIAL_ALLOWED_PATH_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

pub fn serial_path_allowlist_hint() -> String {
    SERIAL_ALLOWED_PATH_PREFIXES
        .iter()
        .map(|prefix| format!("{prefix}*"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn is_dev_sim_serial_path(path: &str) -> bool {
    #[cfg(feature = "dev-sim")]
    {
        path.starts_with(DEV_SIM_SERIAL_PATH_PREFIX)
    }
    #[cfg(not(feature = "dev-sim"))]
    {
        let _ = path;
        false
    }
}

pub fn should_open_serial_nonexclusive(path: &str) -> bool {
    is_dev_sim_serial_path(path)
}

pub fn serial_open_baud(path: &str, configured_baud: u32) -> u32 {
    if is_dev_sim_serial_path(path) {
        0
    } else {
        configured_baud
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_known_serial_path_prefixes() {
        for path in [
            "/dev/ttyACM0",
            "/dev/ttyUSB1",
            "/dev/tty.usbmodem14201",
            "/dev/cu.usbmodem14201",
            "/dev/tty.usbserial-0001",
            "/dev/cu.usbserial-0001",
            "COM3",
        ] {
            assert!(is_serial_path_allowed(path), "{path} should be allowed");
        }
        // The bare prefix is a prefix of itself.
        assert!(is_serial_path_allowed("COM"));
    }

    #[test]
    fn rejects_paths_outside_the_allowlist() {
        for path in [
            "/dev/sda",
            "/dev/ttyS0",
            "/etc/passwd",
            "ttyACM0", // missing /dev/ prefix
            "/dev/ttyXYZ",
            "",
        ] {
            assert!(!is_serial_path_allowed(path), "{path} should be rejected");
        }
    }

    #[test]
    fn allowlist_hint_lists_every_prefix_with_a_glob() {
        let hint = serial_path_allowlist_hint();
        assert!(hint.contains("/dev/ttyACM*"));
        assert!(hint.contains("/dev/ttyUSB*"));
        assert!(hint.contains("COM*"));
        assert!(hint.contains(", "));
    }

    #[test]
    fn non_simulated_paths_open_exclusively_at_configured_baud() {
        // Real device paths are never the dev-sim path, regardless of the
        // `dev-sim` feature, so they open exclusively at the configured baud.
        assert!(!should_open_serial_nonexclusive("/dev/ttyACM0"));
        assert_eq!(serial_open_baud("/dev/ttyACM0", 115_200), 115_200);
        assert_eq!(serial_open_baud("COM3", 9_600), 9_600);
    }
}
