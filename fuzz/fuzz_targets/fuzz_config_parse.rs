#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // Fuzz TOML config parsing — silently discard invalid input
        let _ = toml::from_str::<toml::Value>(s);
    }
});
