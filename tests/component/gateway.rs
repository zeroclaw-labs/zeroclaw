//! Gateway component tests.
//!
//! Tests public gateway constants and configuration validation in isolation.

/// Gateway body limit constant is reasonable.
#[test]
fn gateway_body_limit_is_reasonable() {
    assert_eq!(
        zeroclaw::gateway::MAX_BODY_SIZE,
        65_536,
        "Max body size should be 64KB"
    );
}

/// Gateway timeout constant is reasonable.
#[test]
fn gateway_timeout_is_reasonable() {
    assert_eq!(
        zeroclaw::gateway::REQUEST_TIMEOUT_SECS,
        30,
        "Request timeout should be 30 seconds"
    );
}

/// Gateway rate limit window is 60 seconds.
#[test]
fn gateway_rate_limit_window_is_60s() {
    assert_eq!(
        zeroclaw::gateway::RATE_LIMIT_WINDOW_SECS,
        60,
        "Rate limit window should be 60 seconds"
    );
}
