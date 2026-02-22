use anyhow::Result;
use std::time::Duration;

/// Poll `http://127.0.0.1:{port}/health` until the service responds 200
/// or `timeout_secs` is exceeded.
///
/// Uses exponential backoff starting at 500ms, capped at 4s.
/// Returns `Ok(true)` if healthy before timeout, `Ok(false)` if timed out.
pub async fn poll_health(port: u16, timeout_secs: u64) -> Result<bool> {
    let url = format!("http://127.0.0.1:{}/health", port);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    let mut backoff_ms: u64 = 500;

    loop {
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => return Ok(true),
            _ => {}
        }

        if tokio::time::Instant::now() >= deadline {
            return Ok(false);
        }

        let remaining = deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .as_millis() as u64;
        let sleep_ms = backoff_ms.min(remaining).min(4000);

        if sleep_ms == 0 {
            return Ok(false);
        }

        tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
        backoff_ms = (backoff_ms * 2).min(4000);
    }
}
