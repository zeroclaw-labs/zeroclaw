use crate::parse::{has_cmd, parse_arg};

/// Parsed firmware command.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Ping,
    Capabilities,
    GpioRead { pin: i32 },
    GpioWrite { pin: i32, value: i32 },
}

impl Command {
    /// Parse a raw JSON line into a `Command`.
    ///
    /// Returns `None` for unknown or malformed commands.
    pub fn from_line(line: &[u8]) -> Option<Self> {
        if has_cmd(line, b"ping") {
            Some(Command::Ping)
        } else if has_cmd(line, b"capabilities") {
            Some(Command::Capabilities)
        } else if has_cmd(line, b"gpio_read") {
            let pin = parse_arg(line, b"pin").unwrap_or(-1);
            Some(Command::GpioRead { pin })
        } else if has_cmd(line, b"gpio_write") {
            let pin = parse_arg(line, b"pin").unwrap_or(-1);
            let value = parse_arg(line, b"value").unwrap_or(0);
            Some(Command::GpioWrite { pin, value })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ping() {
        let line = br#"{"id":"1","cmd":"ping"}"#;
        assert_eq!(Command::from_line(line), Some(Command::Ping));
    }

    #[test]
    fn parse_capabilities() {
        let line = br#"{"id":"5","cmd":"capabilities"}"#;
        assert_eq!(Command::from_line(line), Some(Command::Capabilities));
    }

    #[test]
    fn parse_gpio_read() {
        let line = br#"{"id":"3","cmd":"gpio_read","args":{"pin":5}}"#;
        assert_eq!(Command::from_line(line), Some(Command::GpioRead { pin: 5 }));
    }

    #[test]
    fn parse_gpio_write() {
        let line = br#"{"id":"2","cmd":"gpio_write","args":{"pin":13,"value":1}}"#;
        assert_eq!(
            Command::from_line(line),
            Some(Command::GpioWrite {
                pin: 13,
                value: 1
            })
        );
    }

    #[test]
    fn parse_gpio_write_zero() {
        let line = br#"{"id":"2","cmd":"gpio_write","args":{"pin":13,"value":0}}"#;
        assert_eq!(
            Command::from_line(line),
            Some(Command::GpioWrite {
                pin: 13,
                value: 0
            })
        );
    }

    #[test]
    fn parse_unknown_returns_none() {
        let line = br#"{"id":"4","cmd":"reboot"}"#;
        assert_eq!(Command::from_line(line), None);
    }

    #[test]
    fn parse_empty_returns_none() {
        assert_eq!(Command::from_line(b""), None);
    }

    #[test]
    fn parse_gpio_read_missing_pin() {
        let line = br#"{"id":"3","cmd":"gpio_read"}"#;
        assert_eq!(Command::from_line(line), Some(Command::GpioRead { pin: -1 }));
    }
}
