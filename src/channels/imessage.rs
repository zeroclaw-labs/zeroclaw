use crate::channels::traits::{Channel, ChannelMessage};
use async_trait::async_trait;
use directories::UserDirs;
use tokio::sync::mpsc;

/// iMessage channel using macOS `AppleScript` bridge.
/// Polls the Messages database for new messages and sends replies via `osascript`.
#[derive(Clone)]
pub struct IMessageChannel {
    allowed_contacts: Vec<String>,
    poll_interval_secs: u64,
}

impl IMessageChannel {
    pub fn new(allowed_contacts: Vec<String>) -> Self {
        Self {
            allowed_contacts,
            poll_interval_secs: 3,
        }
    }

    fn is_contact_allowed(&self, sender: &str) -> bool {
        if self.allowed_contacts.iter().any(|u| u == "*") {
            return true;
        }
        self.allowed_contacts
            .iter()
            .any(|u| u.eq_ignore_ascii_case(sender))
    }
}

/// Escape a string for safe interpolation into `AppleScript`.
///
/// This prevents injection attacks by escaping:
/// - Backslashes (`\` → `\\`)
/// - Double quotes (`"` → `\"`)
fn escape_applescript(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Validate that a target looks like a valid phone number or email address.
///
/// This is a defense-in-depth measure to reject obviously malicious targets
/// before they reach `AppleScript` interpolation.
///
/// Valid patterns:
/// - Phone: starts with `+` followed by digits (with optional spaces/dashes)
/// - Email: contains `@` with alphanumeric chars on both sides
fn is_valid_imessage_target(target: &str) -> bool {
    let target = target.trim();
    if target.is_empty() {
        return false;
    }

    // Phone number: +1234567890 or +1 234-567-8900
    if target.starts_with('+') {
        let digits_only: String = target.chars().filter(char::is_ascii_digit).collect();
        // Must have at least 7 digits (shortest valid phone numbers)
        return digits_only.len() >= 7 && digits_only.len() <= 15;
    }

    // Email: simple validation (contains @ with chars on both sides)
    if let Some(at_pos) = target.find('@') {
        let local = &target[..at_pos];
        let domain = &target[at_pos + 1..];

        // Local part: non-empty, alphanumeric + common email chars
        let local_valid = !local.is_empty()
            && local
                .chars()
                .all(|c| c.is_alphanumeric() || "._+-".contains(c));

        // Domain: non-empty, contains a dot, alphanumeric + dots/hyphens
        let domain_valid = !domain.is_empty()
            && domain.contains('.')
            && domain
                .chars()
                .all(|c| c.is_alphanumeric() || ".-".contains(c));

        return local_valid && domain_valid;
    }

    false
}

#[async_trait]
impl Channel for IMessageChannel {
    fn name(&self) -> &str {
        "imessage"
    }

    async fn send(&self, message: &str, target: &str) -> anyhow::Result<()> {
        // Defense-in-depth: validate target format before any interpolation
        if !is_valid_imessage_target(target) {
            anyhow::bail!(
                "Invalid iMessage target: must be a phone number (+1234567890) or email (user@example.com)"
            );
        }

        // SECURITY: Escape both message AND target to prevent AppleScript injection
        // See: CWE-78 (OS Command Injection)
        let escaped_msg = escape_applescript(message);
        let escaped_target = escape_applescript(target);

        let script = format!(
            r#"tell application "Messages"
    set targetService to 1st account whose service type = iMessage
    set targetBuddy to participant "{escaped_target}" of targetService
    send "{escaped_msg}" to targetBuddy
end tell"#
        );

        let output = tokio::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("iMessage send failed: {stderr}");
        }

        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        tracing::info!("iMessage channel listening (AppleScript bridge)...");

        // Query the Messages SQLite database for new messages
        // The database is at ~/Library/Messages/chat.db
        let db_path = UserDirs::new()
            .map(|u| u.home_dir().join("Library/Messages/chat.db"))
            .ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?;

        if !db_path.exists() {
            anyhow::bail!(
                "Messages database not found at {}. Ensure Messages.app is set up and Full Disk Access is granted.",
                db_path.display()
            );
        }

        // Track the last ROWID we've seen
        let mut last_rowid = get_max_rowid(&db_path).await.unwrap_or(0);

        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(self.poll_interval_secs)).await;

            let new_messages = fetch_new_messages(&db_path, last_rowid).await;

            match new_messages {
                Ok(messages) => {
                    for (rowid, sender, text) in messages {
                        if rowid > last_rowid {
                            last_rowid = rowid;
                        }

                        if !self.is_contact_allowed(&sender) {
                            continue;
                        }

                        if text.trim().is_empty() {
                            continue;
                        }

                        let msg = ChannelMessage {
                            id: rowid.to_string(),
                            sender: sender.clone(),
                            content: text,
                            channel: "imessage".to_string(),
                            timestamp: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs(),
                        };

                        if tx.send(msg).await.is_err() {
                            return Ok(());
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("iMessage poll error: {e}");
                }
            }
        }
    }

    async fn health_check(&self) -> bool {
        if !cfg!(target_os = "macos") {
            return false;
        }

        let db_path = UserDirs::new()
            .map(|u| u.home_dir().join("Library/Messages/chat.db"))
            .unwrap_or_default();

        db_path.exists()
    }
}

/// Get the current max ROWID from the messages table
async fn get_max_rowid(db_path: &std::path::Path) -> anyhow::Result<i64> {
    let output = tokio::process::Command::new("sqlite3")
        .arg(db_path)
        .arg("SELECT MAX(ROWID) FROM message WHERE is_from_me = 0;")
        .output()
        .await?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let rowid = stdout.trim().parse::<i64>().unwrap_or(0);
    Ok(rowid)
}

/// Fetch messages newer than `since_rowid`
async fn fetch_new_messages(
    db_path: &std::path::Path,
    since_rowid: i64,
) -> anyhow::Result<Vec<(i64, String, String)>> {
    let query = format!(
        "SELECT m.ROWID, h.id, m.text \
         FROM message m \
         JOIN handle h ON m.handle_id = h.ROWID \
         WHERE m.ROWID > {since_rowid} \
         AND m.is_from_me = 0 \
         AND m.text IS NOT NULL \
         ORDER BY m.ROWID ASC \
         LIMIT 20;"
    );

    let output = tokio::process::Command::new("sqlite3")
        .arg("-separator")
        .arg("|")
        .arg(db_path)
        .arg(&query)
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("sqlite3 query failed: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut results = Vec::new();

    for line in stdout.lines() {
        let parts: Vec<&str> = line.splitn(3, '|').collect();
        if parts.len() == 3 {
            if let Ok(rowid) = parts[0].parse::<i64>() {
                results.push((rowid, parts[1].to_string(), parts[2].to_string()));
            }
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_with_contacts() {
        let ch = IMessageChannel::new(vec!["+1234567890".into()]);
        assert_eq!(ch.allowed_contacts.len(), 1);
        assert_eq!(ch.poll_interval_secs, 3);
    }

    #[test]
    fn creates_with_empty_contacts() {
        let ch = IMessageChannel::new(vec![]);
        assert!(ch.allowed_contacts.is_empty());
    }

    #[test]
    fn wildcard_allows_anyone() {
        let ch = IMessageChannel::new(vec!["*".into()]);
        assert!(ch.is_contact_allowed("+1234567890"));
        assert!(ch.is_contact_allowed("random@icloud.com"));
        assert!(ch.is_contact_allowed(""));
    }

    #[test]
    fn specific_contact_allowed() {
        let ch = IMessageChannel::new(vec!["+1234567890".into(), "user@icloud.com".into()]);
        assert!(ch.is_contact_allowed("+1234567890"));
        assert!(ch.is_contact_allowed("user@icloud.com"));
    }

    #[test]
    fn unknown_contact_denied() {
        let ch = IMessageChannel::new(vec!["+1234567890".into()]);
        assert!(!ch.is_contact_allowed("+9999999999"));
        assert!(!ch.is_contact_allowed("hacker@evil.com"));
    }

    #[test]
    fn contact_case_insensitive() {
        let ch = IMessageChannel::new(vec!["User@iCloud.com".into()]);
        assert!(ch.is_contact_allowed("user@icloud.com"));
        assert!(ch.is_contact_allowed("USER@ICLOUD.COM"));
    }

    #[test]
    fn empty_allowlist_denies_all() {
        let ch = IMessageChannel::new(vec![]);
        assert!(!ch.is_contact_allowed("+1234567890"));
        assert!(!ch.is_contact_allowed("anyone"));
    }

    #[test]
    fn name_returns_imessage() {
        let ch = IMessageChannel::new(vec![]);
        assert_eq!(ch.name(), "imessage");
    }

    #[test]
    fn wildcard_among_others_still_allows_all() {
        let ch = IMessageChannel::new(vec!["+111".into(), "*".into(), "+222".into()]);
        assert!(ch.is_contact_allowed("totally-unknown"));
    }

    #[test]
    fn contact_with_spaces_exact_match() {
        let ch = IMessageChannel::new(vec!["  spaced  ".into()]);
        assert!(ch.is_contact_allowed("  spaced  "));
        assert!(!ch.is_contact_allowed("spaced"));
    }

    // ══════════════════════════════════════════════════════════
    // AppleScript Escaping Tests (CWE-78 Prevention)
    // ══════════════════════════════════════════════════════════

    #[test]
    fn escape_applescript_double_quotes() {
        assert_eq!(escape_applescript(r#"hello "world""#), r#"hello \"world\""#);
    }

    #[test]
    fn escape_applescript_backslashes() {
        assert_eq!(escape_applescript(r"path\to\file"), r"path\\to\\file");
    }

    #[test]
    fn escape_applescript_mixed() {
        assert_eq!(
            escape_applescript(r#"say "hello\" world"#),
            r#"say \"hello\\\" world"#
        );
    }

    #[test]
    fn escape_applescript_injection_attempt() {
        // This is the exact attack vector from the security report
        let malicious = r#"" & do shell script "id" & ""#;
        let escaped = escape_applescript(malicious);
        // After escaping, the quotes should be escaped and not break out
        assert_eq!(escaped, r#"\" & do shell script \"id\" & \""#);
        // Verify all quotes are now escaped (preceded by backslash)
        // The escaped string should not have any unescaped quotes (quote not preceded by backslash)
        let chars: Vec<char> = escaped.chars().collect();
        for (i, &c) in chars.iter().enumerate() {
            if c == '"' {
                // Every quote must be preceded by a backslash
                assert!(
                    i > 0 && chars[i - 1] == '\\',
                    "Found unescaped quote at position {i}"
                );
            }
        }
    }

    #[test]
    fn escape_applescript_empty_string() {
        assert_eq!(escape_applescript(""), "");
    }

    #[test]
    fn escape_applescript_no_special_chars() {
        assert_eq!(escape_applescript("hello world"), "hello world");
    }

    #[test]
    fn escape_applescript_unicode() {
        assert_eq!(escape_applescript("hello 🦀 world"), "hello 🦀 world");
    }

    #[test]
    fn escape_applescript_newlines_preserved() {
        assert_eq!(escape_applescript("line1\nline2"), "line1\nline2");
    }

    // ══════════════════════════════════════════════════════════
    // Target Validation Tests
    // ══════════════════════════════════════════════════════════

    #[test]
    fn valid_phone_number_simple() {
        assert!(is_valid_imessage_target("+1234567890"));
    }

    #[test]
    fn valid_phone_number_with_country_code() {
        assert!(is_valid_imessage_target("+14155551234"));
    }

    #[test]
    fn valid_phone_number_with_spaces() {
        assert!(is_valid_imessage_target("+1 415 555 1234"));
    }

    #[test]
    fn valid_phone_number_with_dashes() {
        assert!(is_valid_imessage_target("+1-415-555-1234"));
    }

    #[test]
    fn valid_phone_number_international() {
        assert!(is_valid_imessage_target("+447911123456")); // UK
        assert!(is_valid_imessage_target("+81312345678")); // Japan
    }

    #[test]
    fn valid_email_simple() {
        assert!(is_valid_imessage_target("user@example.com"));
    }

    #[test]
    fn valid_email_with_subdomain() {
        assert!(is_valid_imessage_target("user@mail.example.com"));
    }

    #[test]
    fn valid_email_with_plus() {
        assert!(is_valid_imessage_target("user+tag@example.com"));
    }

    #[test]
    fn valid_email_with_dots() {
        assert!(is_valid_imessage_target("first.last@example.com"));
    }

    #[test]
    fn valid_email_icloud() {
        assert!(is_valid_imessage_target("user@icloud.com"));
        assert!(is_valid_imessage_target("user@me.com"));
    }

    #[test]
    fn invalid_target_empty() {
        assert!(!is_valid_imessage_target(""));
        assert!(!is_valid_imessage_target("   "));
    }

    #[test]
    fn invalid_target_no_plus_prefix() {
        // Phone numbers must start with +
        assert!(!is_valid_imessage_target("1234567890"));
    }

    #[test]
    fn invalid_target_too_short_phone() {
        // Less than 7 digits
        assert!(!is_valid_imessage_target("+123456"));
    }

    #[test]
    fn invalid_target_too_long_phone() {
        // More than 15 digits
        assert!(!is_valid_imessage_target("+1234567890123456"));
    }

    #[test]
    fn invalid_target_email_no_at() {
        assert!(!is_valid_imessage_target("userexample.com"));
    }

    #[test]
    fn invalid_target_email_no_domain() {
        assert!(!is_valid_imessage_target("user@"));
    }

    #[test]
    fn invalid_target_email_no_local() {
        assert!(!is_valid_imessage_target("@example.com"));
    }

    #[test]
    fn invalid_target_email_no_dot_in_domain() {
        assert!(!is_valid_imessage_target("user@localhost"));
    }

    #[test]
    fn invalid_target_injection_attempt() {
        // The exact attack vector from the security report
        assert!(!is_valid_imessage_target(r#"" & do shell script "id" & ""#));
    }

    #[test]
    fn invalid_target_applescript_injection() {
        // Various injection attempts
        assert!(!is_valid_imessage_target(r#"test" & quit"#));
        assert!(!is_valid_imessage_target(r#"test\ndo shell script"#));
        assert!(!is_valid_imessage_target("test\"; malicious code; \""));
    }

    #[test]
    fn invalid_target_special_chars() {
        assert!(!is_valid_imessage_target("user<script>@example.com"));
        assert!(!is_valid_imessage_target("user@example.com; rm -rf /"));
    }

    #[test]
    fn invalid_target_null_byte() {
        assert!(!is_valid_imessage_target("user\0@example.com"));
    }

    #[test]
    fn invalid_target_newline() {
        assert!(!is_valid_imessage_target("user\n@example.com"));
    }

    #[test]
    fn target_with_leading_trailing_whitespace_trimmed() {
        // Should trim and validate
        assert!(is_valid_imessage_target("  +1234567890  "));
        assert!(is_valid_imessage_target("  user@example.com  "));
    }
}
