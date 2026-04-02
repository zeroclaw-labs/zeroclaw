//! Bindings for the Total Phase Aardvark I2C/SPI/GPIO USB adapter.
//!
//! Uses [`libloading`] to load `aardvark.so` at runtime — the same pattern
//! the official Total Phase C stub (`aardvark.c`) uses internally.
//!
//! # Library search order
//!
//! 1. `ZEROCLAW_AARDVARK_LIB` environment variable (full path to `aardvark.so`)
//! 2. `<workspace>/crates/aardvark-sys/vendor/aardvark.so` (development default)
//! 3. `./aardvark.so` (next to the binary, for deployment)
//!
//! If none resolve, every method returns
//! [`Err(AardvarkError::LibraryNotFound)`](AardvarkError::LibraryNotFound).
//!
//! # Safety
//!
//! This crate is the **only** place in ZeroClaw where `unsafe` is permitted.
//! All `unsafe` is confined to `extern "C"` call sites inside this file.
//! The public API is fully safe Rust.

use std::path::PathBuf;
use std::sync::OnceLock;

use libloading::{Library, Symbol};
use thiserror::Error;

// ── Constants from aardvark.h ─────────────────────────────────────────────

/// Bit set on a port returned by `aa_find_devices` when that port is in use.
const AA_PORT_NOT_FREE: u16 = 0x8000;
/// Configure adapter for I2C + GPIO (I2C master mode, SPI disabled).
const AA_CONFIG_GPIO_I2C: i32 = 0x02;
/// Configure adapter for SPI + GPIO (SPI master mode, I2C disabled).
const AA_CONFIG_SPI_GPIO: i32 = 0x01;
/// No I2C flags (standard 7-bit addressing, normal stop condition).
const AA_I2C_NO_FLAGS: i32 = 0x00;
/// Enable both onboard I2C pullup resistors (hardware v2+ only).
const AA_I2C_PULLUP_BOTH: u8 = 0x03;

// ── Library loading ───────────────────────────────────────────────────────

static AARDVARK_LIB: OnceLock<Option<Library>> = OnceLock::new();

fn lib() -> Option<&'static Library> {
    AARDVARK_LIB
        .get_or_init(|| {
            let candidates: Vec<PathBuf> = vec![
                // 1. Explicit env-var override (full path)
                std::env::var("ZEROCLAW_AARDVARK_LIB")
                    .ok()
                    .map(PathBuf::from)
                    .unwrap_or_default(),
                // 2. Vendor directory shipped with this crate (dev default)
                {
                    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
                    p.push("vendor/aardvark.so");
                    p
                },
                // 3. Next to the running binary (deployment)
                std::env::current_exe()
                    .ok()
                    .and_then(|e| e.parent().map(|d| d.join("aardvark.so")))
                    .unwrap_or_default(),
                // 4. Current working directory
                PathBuf::from("aardvark.so"),
            ];
            let mut tried_any = false;
            for path in &candidates {
                if path.as_os_str().is_empty() {
                    continue;
                }
                tried_any = true;
                match unsafe { Library::new(path) } {
                    Ok(lib) => {
                        // Verify the .so exports aa_c_version (Total Phase version gate).
                        // The .so exports c_aa_* symbols (not aa_*); aa_c_version is the
                        // one non-prefixed symbol used to confirm library identity.
                        let version_ok = unsafe {
                            lib.get::<unsafe extern "C" fn() -> u32>(b"aa_c_version\0").is_ok()
                        };
                        if !version_ok {
                            eprintln!(
                                "[aardvark-sys] {} loaded but aa_c_version not found — \
                                 not a valid Aardvark library, skipping",
                                path.display()
                            );
                            continue;
                        }
                        eprintln!("[aardvark-sys] loaded library from {}", path.display());
                        return Some(lib);
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        // Surface architecture mismatch explicitly — the most common
                        // failure on Apple Silicon machines with an x86_64 SDK.
                        if msg.contains("incompatible architecture") || msg.contains("mach-o file") {
                            eprintln!(
                                "[aardvark-sys] ARCHITECTURE MISMATCH loading {}: {}\n\
                                 [aardvark-sys] The vendored aardvark.so is x86_64 but this \
                                 binary is {}.\n\
                                 [aardvark-sys] Download the arm64 SDK from https://www.totalphase.com/downloads/ \
                                 or build with --target x86_64-apple-darwin.",
                                path.display(),
                                msg,
                                std::env::consts::ARCH,
                            );
                        } else {
                            eprintln!(
                                "[aardvark-sys] could not load {}: {}",
                                path.display(),
                                msg
                            );
                        }
                    }
                }
            }
            if !tried_any {
                eprintln!("[aardvark-sys] no library candidates found; set ZEROCLAW_AARDVARK_LIB or place aardvark.so next to the binary");
            }
            None
        })
        .as_ref()
}

/// Errors returned by Aardvark hardware operations.
#[derive(Debug, Error)]
pub enum AardvarkError {
    /// No Aardvark adapter found — adapter not plugged in.
    #[error("Aardvark adapter not found — is it plugged in?")]
    NotFound,
    /// `aa_open` returned a non-positive handle.
    #[error("Aardvark open failed (code {0})")]
    OpenFailed(i32),
    /// `aa_i2c_write` returned a negative status code.
    #[error("I2C write failed (code {0})")]
    I2cWriteFailed(i32),
    /// `aa_i2c_read` returned a negative status code.
    #[error("I2C read failed (code {0})")]
    I2cReadFailed(i32),
    /// `aa_spi_write` returned a negative status code.
    #[error("SPI transfer failed (code {0})")]
    SpiTransferFailed(i32),
    /// GPIO operation returned a negative status code.
    #[error("GPIO error (code {0})")]
    GpioError(i32),
    /// `aardvark.so` could not be found or loaded.
    #[error("aardvark.so not found — set ZEROCLAW_AARDVARK_LIB or place it next to the binary")]
    LibraryNotFound,
}

/// Convenience `Result` alias for this crate.
pub type Result<T> = std::result::Result<T, AardvarkError>;

// ── Handle ────────────────────────────────────────────────────────────────

/// Safe RAII handle over the Aardvark C library handle.
///
/// Automatically closes the adapter on `Drop`.
///
/// **Usage pattern:** open a fresh handle per command and let it drop at the
/// end of each operation (lazy-open / eager-close).
pub struct AardvarkHandle {
    handle: i32,
}

impl AardvarkHandle {
    // ── Lifecycle ─────────────────────────────────────────────────────────

    /// Open the first available (free) Aardvark adapter.
    pub fn open() -> Result<Self> {
        let ports = Self::find_devices();
        let port = ports.first().copied().ok_or(AardvarkError::NotFound)?;
        Self::open_port(i32::from(port))
    }

    /// Open a specific Aardvark adapter by port index.
    pub fn open_port(port: i32) -> Result<Self> {
        let lib = lib().ok_or(AardvarkError::LibraryNotFound)?;
        let handle: i32 = unsafe {
            let f: Symbol<unsafe extern "C" fn(i32) -> i32> = lib
                .get(b"c_aa_open\0")
                .map_err(|_| AardvarkError::LibraryNotFound)?;
            f(port)
        };
        if handle <= 0 {
            Err(AardvarkError::OpenFailed(handle))
        } else {
            Ok(Self { handle })
        }
    }

    /// Return the port numbers of all **free** connected adapters.
    ///
    /// Ports in-use by another process are filtered out.
    /// Returns an empty `Vec` when `aardvark.so` cannot be loaded.
    pub fn find_devices() -> Vec<u16> {
        let Some(lib) = lib() else {
            eprintln!("[aardvark-sys] find_devices: library not loaded");
            return Vec::new();
        };
        let mut ports = [0u16; 16];
        let n: i32 = unsafe {
            let f: std::result::Result<Symbol<unsafe extern "C" fn(i32, *mut u16) -> i32>, _> =
                lib.get(b"c_aa_find_devices\0");
            match f {
                Ok(f) => f(16, ports.as_mut_ptr()),
                Err(e) => {
                    eprintln!("[aardvark-sys] find_devices: symbol lookup failed: {e}");
                    return Vec::new();
                }
            }
        };
        eprintln!(
            "[aardvark-sys] find_devices: c_aa_find_devices returned {n}, ports={:?}",
            &ports[..n.max(0) as usize]
        );
        if n <= 0 {
            return Vec::new();
        }
        let free: Vec<u16> = ports[..n as usize]
            .iter()
            .filter(|&&p| (p & AA_PORT_NOT_FREE) == 0)
            .copied()
            .collect();
        eprintln!("[aardvark-sys] find_devices: free ports={free:?}");
        free
    }

    // ── I2C ───────────────────────────────────────────────────────────────

    /// Enable I2C mode and set the bitrate (kHz).
    pub fn i2c_enable(&self, bitrate_khz: u32) -> Result<()> {
        let lib = lib().ok_or(AardvarkError::LibraryNotFound)?;
        unsafe {
            let configure: Symbol<unsafe extern "C" fn(i32, i32) -> i32> = lib
                .get(b"c_aa_configure\0")
                .map_err(|_| AardvarkError::LibraryNotFound)?;
            configure(self.handle, AA_CONFIG_GPIO_I2C);
            let pullup: Symbol<unsafe extern "C" fn(i32, u8) -> i32> = lib
                .get(b"c_aa_i2c_pullup\0")
                .map_err(|_| AardvarkError::LibraryNotFound)?;
            pullup(self.handle, AA_I2C_PULLUP_BOTH);
            let bitrate: Symbol<unsafe extern "C" fn(i32, i32) -> i32> = lib
                .get(b"c_aa_i2c_bitrate\0")
                .map_err(|_| AardvarkError::LibraryNotFound)?;
            bitrate(self.handle, bitrate_khz as i32);
        }
        Ok(())
    }

    /// Write `data` bytes to the I2C device at `addr`.
    pub fn i2c_write(&self, addr: u8, data: &[u8]) -> Result<()> {
        let lib = lib().ok_or(AardvarkError::LibraryNotFound)?;
        let ret: i32 = unsafe {
            let f: Symbol<unsafe extern "C" fn(i32, u16, i32, u16, *const u8) -> i32> = lib
                .get(b"c_aa_i2c_write\0")
                .map_err(|_| AardvarkError::LibraryNotFound)?;
            f(
                self.handle,
                u16::from(addr),
                AA_I2C_NO_FLAGS,
                data.len() as u16,
                data.as_ptr(),
            )
        };
        if ret < 0 {
            Err(AardvarkError::I2cWriteFailed(ret))
        } else {
            Ok(())
        }
    }

    /// Read `len` bytes from the I2C device at `addr`.
    pub fn i2c_read(&self, addr: u8, len: usize) -> Result<Vec<u8>> {
        let lib = lib().ok_or(AardvarkError::LibraryNotFound)?;
        let mut buf = vec![0u8; len];
        let ret: i32 = unsafe {
            let f: Symbol<unsafe extern "C" fn(i32, u16, i32, u16, *mut u8) -> i32> = lib
                .get(b"c_aa_i2c_read\0")
                .map_err(|_| AardvarkError::LibraryNotFound)?;
            f(
                self.handle,
                u16::from(addr),
                AA_I2C_NO_FLAGS,
                len as u16,
                buf.as_mut_ptr(),
            )
        };
        if ret < 0 {
            Err(AardvarkError::I2cReadFailed(ret))
        } else {
            Ok(buf)
        }
    }

    /// Write then read — standard I2C register-read pattern.
    pub fn i2c_write_read(&self, addr: u8, write_data: &[u8], read_len: usize) -> Result<Vec<u8>> {
        self.i2c_write(addr, write_data)?;
        self.i2c_read(addr, read_len)
    }

    /// Scan the I2C bus, returning addresses of all responding devices.
    ///
    /// Probes `0x08–0x77` with a 1-byte read; returns addresses that ACK.
    pub fn i2c_scan(&self) -> Vec<u8> {
        let Some(lib) = lib() else {
            return Vec::new();
        };
        let Ok(f): std::result::Result<
            Symbol<unsafe extern "C" fn(i32, u16, i32, u16, *mut u8) -> i32>,
            _,
        > = (unsafe { lib.get(b"c_aa_i2c_read\0") }) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        let mut buf = [0u8; 1];
        for addr in 0x08u16..=0x77 {
            let ret = unsafe { f(self.handle, addr, AA_I2C_NO_FLAGS, 1, buf.as_mut_ptr()) };
            // ret > 0: bytes received → device ACKed
            // ret == 0: NACK → no device at this address
            // ret < 0: error code → skip
            if ret > 0 {
                found.push(addr as u8);
            }
        }
        found
    }

    // ── SPI ───────────────────────────────────────────────────────────────

    /// Enable SPI mode and set the bitrate (kHz).
    pub fn spi_enable(&self, bitrate_khz: u32) -> Result<()> {
        let lib = lib().ok_or(AardvarkError::LibraryNotFound)?;
        unsafe {
            let configure: Symbol<unsafe extern "C" fn(i32, i32) -> i32> = lib
                .get(b"c_aa_configure\0")
                .map_err(|_| AardvarkError::LibraryNotFound)?;
            configure(self.handle, AA_CONFIG_SPI_GPIO);
            // SPI mode 0: polarity=rising/falling(0), phase=sample/setup(0), MSB first(0)
            let spi_cfg: Symbol<unsafe extern "C" fn(i32, i32, i32, i32) -> i32> = lib
                .get(b"c_aa_spi_configure\0")
                .map_err(|_| AardvarkError::LibraryNotFound)?;
            spi_cfg(self.handle, 0, 0, 0);
            let bitrate: Symbol<unsafe extern "C" fn(i32, i32) -> i32> = lib
                .get(b"c_aa_spi_bitrate\0")
                .map_err(|_| AardvarkError::LibraryNotFound)?;
            bitrate(self.handle, bitrate_khz as i32);
        }
        Ok(())
    }

    /// Full-duplex SPI transfer.
    ///
    /// Sends `send` bytes; returns the simultaneously received bytes (same length).
    pub fn spi_transfer(&self, send: &[u8]) -> Result<Vec<u8>> {
        let lib = lib().ok_or(AardvarkError::LibraryNotFound)?;
        let mut recv = vec![0u8; send.len()];
        // aa_spi_write(aardvark, out_num_bytes, data_out, in_num_bytes, data_in)
        let ret: i32 = unsafe {
            let f: Symbol<unsafe extern "C" fn(i32, u16, *const u8, u16, *mut u8) -> i32> = lib
                .get(b"c_aa_spi_write\0")
                .map_err(|_| AardvarkError::LibraryNotFound)?;
            f(
                self.handle,
                send.len() as u16,
                send.as_ptr(),
                recv.len() as u16,
                recv.as_mut_ptr(),
            )
        };
        if ret < 0 {
            Err(AardvarkError::SpiTransferFailed(ret))
        } else {
            Ok(recv)
        }
    }

    // ── GPIO ──────────────────────────────────────────────────────────────

    /// Set GPIO pin directions and output values.
    ///
    /// `direction`: bitmask — `1` = output, `0` = input.
    /// `value`: output state bitmask.
    pub fn gpio_set(&self, direction: u8, value: u8) -> Result<()> {
        let lib = lib().ok_or(AardvarkError::LibraryNotFound)?;
        unsafe {
            let dir_f: Symbol<unsafe extern "C" fn(i32, u8) -> i32> = lib
                .get(b"c_aa_gpio_direction\0")
                .map_err(|_| AardvarkError::LibraryNotFound)?;
            let d = dir_f(self.handle, direction);
            if d < 0 {
                return Err(AardvarkError::GpioError(d));
            }
            let set_f: Symbol<unsafe extern "C" fn(i32, u8) -> i32> =
                lib.get(b"c_aa_gpio_set\0")
                    .map_err(|_| AardvarkError::LibraryNotFound)?;
            let r = set_f(self.handle, value);
            if r < 0 {
                return Err(AardvarkError::GpioError(r));
            }
        }
        Ok(())
    }

    /// Read the current GPIO pin states as a bitmask.
    pub fn gpio_get(&self) -> Result<u8> {
        let lib = lib().ok_or(AardvarkError::LibraryNotFound)?;
        let ret: i32 = unsafe {
            let f: Symbol<unsafe extern "C" fn(i32) -> i32> = lib
                .get(b"c_aa_gpio_get\0")
                .map_err(|_| AardvarkError::LibraryNotFound)?;
            f(self.handle)
        };
        if ret < 0 {
            Err(AardvarkError::GpioError(ret))
        } else {
            Ok(ret as u8)
        }
    }
}

impl Drop for AardvarkHandle {
    fn drop(&mut self) {
        if let Some(lib) = lib() {
            unsafe {
                if let Ok(f) = lib.get::<unsafe extern "C" fn(i32) -> i32>(b"c_aa_close\0") {
                    f(self.handle);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_devices_does_not_panic() {
        // With no adapter plugged in, must return empty without panicking.
        let _ = AardvarkHandle::find_devices();
    }

    #[test]
    fn open_returns_error_or_ok_depending_on_hardware() {
        // With hardware connected: open() succeeds (Ok).
        // Without hardware: returns LibraryNotFound, NotFound, or OpenFailed — any Err is fine.
        // Both outcomes are valid; the important thing is no panic.
        let _ = AardvarkHandle::open();
    }

    #[test]
    fn open_port_returns_error_when_no_hardware() {
        // Port 99 doesn't exist — must return an error regardless of whether hardware is connected.
        assert!(AardvarkHandle::open_port(99).is_err());
    }

    #[test]
    fn error_display_messages_are_human_readable() {
        assert!(
            AardvarkError::NotFound
                .to_string()
                .to_lowercase()
                .contains("not found")
        );
        assert!(AardvarkError::OpenFailed(-1).to_string().contains("-1"));
        assert!(
            AardvarkError::I2cWriteFailed(-3)
                .to_string()
                .contains("I2C write")
        );
        assert!(
            AardvarkError::SpiTransferFailed(-2)
                .to_string()
                .contains("SPI")
        );
        assert!(
            AardvarkError::LibraryNotFound
                .to_string()
                .contains("aardvark.so")
        );
    }
}
