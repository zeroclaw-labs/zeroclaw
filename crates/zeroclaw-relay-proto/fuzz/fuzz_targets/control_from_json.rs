//! Fuzz the relay control-frame parser (A12): `Control::from_json` must never
//! panic on arbitrary input, and any frame it accepts must round-trip through
//! `to_json` -> `from_json` to the same value (no parse/serialize asymmetry an
//! attacker could exploit to smuggle a different frame past one side).
#![no_main]

use libfuzzer_sys::fuzz_target;
use zeroclaw_relay_proto::Control;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        if let Ok(frame) = Control::from_json(s) {
            let reparsed = Control::from_json(&frame.to_json())
                .expect("a frame we produced must re-parse");
            assert_eq!(frame, reparsed, "control frame did not round-trip");
        }
    }
});
