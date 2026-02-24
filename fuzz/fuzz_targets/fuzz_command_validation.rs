#![no_main]
#![forbid(unsafe_code)]
use libfuzzer_sys::fuzz_target;
use zeroclaw::security::SecurityPolicy;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let policy = SecurityPolicy::default();
        let _ = policy.validate_command_execution(s, false);
    }
});
