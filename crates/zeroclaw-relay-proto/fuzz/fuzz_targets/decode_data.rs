//! Fuzz the binary DATA frame decoder + the flow-control helpers (A12). Neither
//! `decode_data` nor `ConnWindow` may panic on arbitrary bytes, and
//! `encode_data(decode) == identity` for any frame that decodes.
#![no_main]

use libfuzzer_sys::fuzz_target;
use zeroclaw_relay_proto::{ConnWindow, decode_data, encode_data};

fuzz_target!(|data: &[u8]| {
    if let Some((conn_id, payload)) = decode_data(data) {
        // Re-encoding the decoded parts reproduces the original frame.
        assert_eq!(encode_data(conn_id, payload), data);
    }
    // The credit accountant must stay panic-free under arbitrary debits.
    let mut w = ConnWindow::new(zeroclaw_relay_proto::INITIAL_WINDOW);
    w.debit(data.len());
    let _ = w.overrun();
    let _ = w.is_blocked();
});
