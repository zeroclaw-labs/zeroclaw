//! Fuzz the binary DATA frame decoder + the flow-control helpers (A12). None of
//! `decode_data`, `chunk_payload`, or `ConnWindow` may panic on arbitrary bytes,
//! and `encode_data(decode) == identity` for any frame that decodes.
#![no_main]

use libfuzzer_sys::fuzz_target;
use zeroclaw_relay_proto::{ConnWindow, chunk_payload, decode_data, encode_data};

fuzz_target!(|data: &[u8]| {
    if let Some((conn_id, payload)) = decode_data(data) {
        // Re-encoding the decoded parts reproduces the original frame.
        assert_eq!(encode_data(conn_id, payload), data);
    }
    // chunk_payload never yields an over-sized chunk and never panics.
    for chunk in chunk_payload(data) {
        assert!(chunk.len() <= zeroclaw_relay_proto::MAX_DATA_PAYLOAD);
    }
    // The credit accountant must stay panic-free under arbitrary debits.
    let mut w = ConnWindow::new(zeroclaw_relay_proto::INITIAL_WINDOW);
    w.debit(data.len());
    let _ = w.overrun();
    let _ = w.is_blocked();
});
