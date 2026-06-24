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
