#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // Fuzz TOML config parsing into real ZeroClaw Config struct
        let _ = toml::from_str::<zeroclaw::Config>(s);
    }
});
