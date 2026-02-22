use anyhow::Result;

use crate::db::pool::DbPool;
use crate::docker::DockerManager;

/// Extract the most recent pairing code from container logs.
/// Looks for the pattern `X-Pairing-Code: XXXXXX` in the log output.
pub fn extract_pairing_code(logs: &str) -> Option<String> {
    // Find the LAST occurrence of the pairing code pattern
    let mut last_code = None;
    for line in logs.lines() {
        if let Some(pos) = line.find("X-Pairing-Code: ") {
            let after = &line[pos + "X-Pairing-Code: ".len()..];
            let code: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
            if code.len() == 6 {
                last_code = Some(code);
            }
        }
    }
    last_code
}

/// Read pairing code from container logs and store it in the DB.
///
/// Retries up to 3 times with 2s delay because the pairing code
/// may not appear in logs immediately after container startup.
/// Does NOT consume the code — the end user enters it in the
/// ZeroClaw web dashboard to pair their browser.
pub async fn read_and_store_pairing_code(
    docker: &DockerManager,
    db: &DbPool,
    tenant_id: &str,
    slug: &str,
) -> Result<Option<String>> {
    let mut code = None;

    for attempt in 0..3 {
        let logs = docker.logs(slug, 50)?;
        if let Some(c) = extract_pairing_code(&logs) {
            code = Some(c);
            break;
        }
        if attempt < 2 {
            tracing::debug!("pairing code not yet in logs for {} (attempt {}), retrying...", slug, attempt + 1);
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }

    let code = match code {
        Some(c) => c,
        None => {
            tracing::debug!("no pairing code found in logs for {} after 3 attempts", slug);
            return Ok(None);
        }
    };

    tracing::info!("stored pairing code for tenant {}", slug);

    db.write(|conn| {
        conn.execute(
            "UPDATE tenants SET pairing_code = ?1 WHERE id = ?2",
            rusqlite::params![code, tenant_id],
        )?;
        Ok(())
    })?;

    Ok(Some(code))
}

/// Clear the stored pairing code from DB (e.g., after user has paired).
pub fn clear_pairing_code(db: &DbPool, tenant_id: &str) -> Result<()> {
    db.write(|conn| {
        conn.execute(
            "UPDATE tenants SET pairing_code = NULL WHERE id = ?1",
            rusqlite::params![tenant_id],
        )?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_pairing_code_from_logs() {
        let logs = r#"
ZeroClaw Gateway listening on http://0.0.0.0:10001
  POST /pair      — pair a new client (X-Pairing-Code header)
  GET  /health    — health check

  PAIRING REQUIRED — use this one-time code:
     ┌──────────────┐
     │  870465  │
     └──────────────┘
     Send: POST /pair with header X-Pairing-Code: 870465
  Press Ctrl+C to stop.
"#;
        assert_eq!(extract_pairing_code(logs), Some("870465".to_string()));
    }

    #[test]
    fn test_extract_pairing_code_takes_last_occurrence() {
        let logs = "X-Pairing-Code: 111111\nX-Pairing-Code: 222222\n";
        assert_eq!(extract_pairing_code(logs), Some("222222".to_string()));
    }

    #[test]
    fn test_extract_pairing_code_no_match() {
        let logs = "no pairing code here\njust normal logs\n";
        assert_eq!(extract_pairing_code(logs), None);
    }
}
