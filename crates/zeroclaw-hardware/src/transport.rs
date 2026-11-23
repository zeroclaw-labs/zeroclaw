/// Transport kind discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransportKind {
    /// Newline-delimited JSON over USB CDC serial.
    Serial,
    /// SWD debug probe (probe-rs).
    Swd,
    /// Direct Linux GPIO/I2C/SPI (rppal, sysfs).
    Native,
}

impl std::fmt::Display for TransportKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Serial => write!(f, "serial"),
            Self::Swd => write!(f, "swd"),
            Self::Native => write!(f, "native"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_kind_display() {
        assert_eq!(TransportKind::Serial.to_string(), "serial");
        assert_eq!(TransportKind::Swd.to_string(), "swd");
        assert_eq!(TransportKind::Native.to_string(), "native");
    }

    #[test]
    fn transport_kind_equality() {
        assert_eq!(TransportKind::Serial, TransportKind::Serial);
        assert_ne!(TransportKind::Serial, TransportKind::Swd);
    }
}
