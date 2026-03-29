use crate::channels::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use directories::UserDirs;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use tokio::sync::mpsc;

/// Extract plain text from an iMessage `attributedBody` typedstream blob.
///
/// Modern macOS (Ventura+) stores message content in `attributedBody` as an
/// `NSMutableAttributedString` serialized via Apple's typedstream format,
/// rather than the plain `text` column.
///
/// This follows the well-documented marker-based approach used by LangChain,
/// steipete/imsg, and mac_apt (all MIT-licensed). See:
/// <https://chrissardegna.com/blog/reverse-engineering-apples-typedstream-format/>
fn extract_text_from_attributed_body(blob: &[u8]) -> Option<String> {
    // Find the start-of-text marker: [0x01, 0x2B]
    // 0x2B is the C-string type tag in Apple's typedstream format.
    let marker_pos = blob.windows(2).position(|w| w == [0x01, 0x2B])?;
    let rest = blob.get(marker_pos + 2..)?;

    if rest.is_empty() {
        return None;
    }

    // Read variable-length prefix immediately after the marker.
    // The length determines text extent — we do NOT scan for an end marker,
    // because byte pairs like [0x86, 0x84] can appear inside valid UTF-8
    // (e.g. U+2184 LATIN SMALL LETTER REVERSED C encodes to E2 86 84).
    //
    //   0x00-0x7F => literal length (1 byte)
    //   0x81      => next 2 bytes are little-endian u16 length
    //   0x82      => next 4 bytes are little-endian u32 length
    //   0x80, 0x83+ are not observed in iMessage typedstreams; reject gracefully.
    let (length, text_start) = match rest[0] {
        0x81 if rest.len() >= 3 => {
            let len = u16::from_le_bytes([rest[1], rest[2]]) as usize;
            (len, 3)
        }
        0x82 if rest.len() >= 5 => {
            let len = u32::from_le_bytes([rest[1], rest[2], rest[3], rest[4]]) as usize;
            (len, 5)
        }
        b if b <= 0x7F => (b as usize, 1),
        _ => return None,
    };

    let text_bytes = rest.get(text_start..text_start + length)?;
    std::str::from_utf8(text_bytes).ok().map(str::to_owned)
}

/// Resolve message content from the `text` column with `attributedBody` fallback.
///
/// Prefers the plain `text` column when present. Falls back to parsing the
/// typedstream blob in `attributedBody` (modern macOS). Logs a warning when
/// `attributedBody` exists but cannot be parsed.
fn resolve_message_content(rowid: i64, text: Option<String>, body: Option<Vec<u8>>) -> String {
    text.filter(|t| !t.trim().is_empty())
        .or_else(|| {
            let parsed = body.as_deref().and_then(extract_text_from_attributed_body);
            if parsed.is_none() && body.as_ref().is_some_and(|b| !b.is_empty()) {
                tracing::warn!(rowid, "failed to parse attributedBody");
            }
            parsed
        })
        .unwrap_or_default()
}

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
/// - Newlines (`\n` → `\\n`, `\r` → `\\r`) to prevent code injection via line breaks
fn escape_applescript(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
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

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        // Defense-in-depth: validate target format before any interpolation
        if !is_valid_imessage_target(&message.recipient) {
            anyhow::bail!(
                "Invalid iMessage target: must be a phone number (+1234567890) or email (user@example.com)"
            );
        }

        // SECURITY: Escape both message AND target to prevent AppleScript injection
        // See: CWE-78 (OS Command Injection)
        let escaped_msg = escape_applescript(&message.content);
        let escaped_target = escape_applescript(&message.recipient);

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

        // Open a persistent read-only connection instead of creating
        // a new one on every 3-second poll cycle.
        let path = db_path.to_path_buf();
        let conn = tokio::task::spawn_blocking(move || -> anyhow::Result<Connection> {
            Ok(Connection::open_with_flags(
                &path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?)
        })
        .await??;

        // Track the last ROWID we've seen (shuttle conn in and out)
        let (mut conn, initial_rowid) =
            tokio::task::spawn_blocking(move || -> anyhow::Result<(Connection, i64)> {
                let rowid = {
                    let mut stmt =
                        conn.prepare("SELECT MAX(ROWID) FROM message WHERE is_from_me = 0")?;
                    let rowid: Option<i64> = stmt.query_row([], |row| row.get(0))?;
                    rowid.unwrap_or(0)
                };
                Ok((conn, rowid))
            })
            .await??;
        let mut last_rowid = initial_rowid;

        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(self.poll_interval_secs)).await;

            let since = last_rowid;
            let (returned_conn, poll_result) = tokio::task::spawn_blocking(
                move || -> (Connection, anyhow::Result<Vec<(i64, String, String)>>) {
                    let result = (|| -> anyhow::Result<Vec<(i64, String, String)>> {
                        let mut stmt = conn.prepare(
                            "SELECT m.ROWID, h.id, m.text, m.attributedBody \
                     FROM message m \
                     JOIN handle h ON m.handle_id = h.ROWID \
                     WHERE m.ROWID > ?1 \
                     AND m.is_from_me = 0 \
                     AND (m.text IS NOT NULL OR m.attributedBody IS NOT NULL) \
                     ORDER BY m.ROWID ASC \
                     LIMIT 20",
                        )?;
                        let rows = stmt.query_map([since], |row| {
                            let rowid = row.get::<_, i64>(0)?;
                            let sender = row.get::<_, String>(1)?;
                            let text: Option<String> = row.get(2)?;
                            let body: Option<Vec<u8>> = row.get(3)?;
                            Ok((rowid, sender, resolve_message_content(rowid, text, body)))
                        })?;
                        let results = rows.collect::<Result<Vec<_>, _>>()?;
                        Ok(results)
                    })();

                    (conn, result)
                },
            )
            .await
            .map_err(|e| anyhow::anyhow!("iMessage poll worker join error: {e}"))?;
            conn = returned_conn;

            match poll_result {
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
                            reply_target: sender.clone(),
                            content: text,
                            channel: "imessage".to_string(),
                            timestamp: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs(),
                            thread_ts: None,
                            reply_to_message_id: None,
                            interruption_scope_id: None,
                            attachments: vec![],
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

/// Get the current max ROWID from the messages table.
/// Uses rusqlite with parameterized queries for security (CWE-89 prevention).
async fn get_max_rowid(db_path: &Path) -> anyhow::Result<i64> {
    let path = db_path.to_path_buf();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<i64> {
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let mut stmt = conn.prepare("SELECT MAX(ROWID) FROM message WHERE is_from_me = 0")?;
        let rowid: Option<i64> = stmt.query_row([], |row| row.get(0))?;
        Ok(rowid.unwrap_or(0))
    })
    .await??;
    Ok(result)
}

/// Fetch messages newer than `since_rowid`.
/// Uses rusqlite with parameterized queries for security (CWE-89 prevention).
/// The `since_rowid` parameter is bound safely, preventing SQL injection.
async fn fetch_new_messages(
    db_path: &Path,
    since_rowid: i64,
) -> anyhow::Result<Vec<(i64, String, String)>> {
    let path = db_path.to_path_buf();
    let results =
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<(i64, String, String)>> {
            let conn = Connection::open_with_flags(
                &path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            let mut stmt = conn.prepare(
                "SELECT m.ROWID, h.id, m.text, m.attributedBody \
             FROM message m \
             JOIN handle h ON m.handle_id = h.ROWID \
             WHERE m.ROWID > ?1 \
             AND m.is_from_me = 0 \
             AND (m.text IS NOT NULL OR m.attributedBody IS NOT NULL) \
             ORDER BY m.ROWID ASC \
             LIMIT 20",
            )?;
            let rows = stmt.query_map([since_rowid], |row| {
                let rowid = row.get::<_, i64>(0)?;
                let sender = row.get::<_, String>(1)?;
                let text: Option<String> = row.get(2)?;
                let body: Option<Vec<u8>> = row.get(3)?;
                Ok((rowid, sender, resolve_message_content(rowid, text, body)))
            })?;
            let results: Vec<_> = rows
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|(_, _, content)| !content.trim().is_empty())
                .collect();
            Ok(results)
        })
        .await??;
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
    fn escape_applescript_newlines_escaped() {
        assert_eq!(escape_applescript("line1\nline2"), "line1\\nline2");
        assert_eq!(escape_applescript("line1\rline2"), "line1\\rline2");
        assert_eq!(escape_applescript("line1\r\nline2"), "line1\\r\\nline2");
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
        assert!(!is_valid_imessage_target(r"test\ndo shell script"));
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

    // ══════════════════════════════════════════════════════════
    // SQLite/rusqlite Database Tests (CWE-89 Prevention)
    // ══════════════════════════════════════════════════════════

    /// Helper to create a temporary test database with Messages schema
    fn create_test_db() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("chat.db");

        let conn = Connection::open(&db_path).unwrap();

        // Create minimal schema matching macOS Messages.app
        conn.execute_batch(
            "CREATE TABLE handle (
                ROWID INTEGER PRIMARY KEY,
                id TEXT NOT NULL
            );
            CREATE TABLE message (
                ROWID INTEGER PRIMARY KEY,
                handle_id INTEGER,
                text TEXT,
                attributedBody BLOB,
                is_from_me INTEGER DEFAULT 0,
                FOREIGN KEY (handle_id) REFERENCES handle(ROWID)
            );",
        )
        .unwrap();

        (dir, db_path)
    }

    #[tokio::test]
    async fn get_max_rowid_empty_database() {
        let (_dir, db_path) = create_test_db();
        let result = get_max_rowid(&db_path).await;
        assert!(result.is_ok());
        // Empty table returns 0 (NULL coalesced)
        assert_eq!(result.unwrap(), 0);
    }

    #[tokio::test]
    async fn get_max_rowid_with_messages() {
        let (_dir, db_path) = create_test_db();

        // Insert test data
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute(
                "INSERT INTO handle (ROWID, id) VALUES (1, '+1234567890')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO message (ROWID, handle_id, text, is_from_me) VALUES (100, 1, 'Hello', 0)",
                []
            ).unwrap();
            conn.execute(
                "INSERT INTO message (ROWID, handle_id, text, is_from_me) VALUES (200, 1, 'World', 0)",
                []
            ).unwrap();
            // This one is from_me=1, should be ignored
            conn.execute(
                "INSERT INTO message (ROWID, handle_id, text, is_from_me) VALUES (300, 1, 'Sent', 1)",
                []
            ).unwrap();
        }

        let result = get_max_rowid(&db_path).await.unwrap();
        // Should return 200, not 300 (ignores is_from_me=1)
        assert_eq!(result, 200);
    }

    #[tokio::test]
    async fn get_max_rowid_nonexistent_database() {
        let path = std::path::Path::new("/nonexistent/path/chat.db");
        let result = get_max_rowid(path).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn fetch_new_messages_empty_database() {
        let (_dir, db_path) = create_test_db();
        let result = fetch_new_messages(&db_path, 0).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[tokio::test]
    async fn fetch_new_messages_returns_correct_data() {
        let (_dir, db_path) = create_test_db();

        // Insert test data
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute(
                "INSERT INTO handle (ROWID, id) VALUES (1, '+1234567890')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO handle (ROWID, id) VALUES (2, 'user@example.com')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO message (ROWID, handle_id, text, is_from_me) VALUES (10, 1, 'First message', 0)",
                []
            ).unwrap();
            conn.execute(
                "INSERT INTO message (ROWID, handle_id, text, is_from_me) VALUES (20, 2, 'Second message', 0)",
                []
            ).unwrap();
        }

        let result = fetch_new_messages(&db_path, 0).await.unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(
            result[0],
            (10, "+1234567890".to_string(), "First message".to_string())
        );
        assert_eq!(
            result[1],
            (
                20,
                "user@example.com".to_string(),
                "Second message".to_string()
            )
        );
    }

    #[tokio::test]
    async fn fetch_new_messages_filters_by_rowid() {
        let (_dir, db_path) = create_test_db();

        // Insert test data
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute(
                "INSERT INTO handle (ROWID, id) VALUES (1, '+1234567890')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO message (ROWID, handle_id, text, is_from_me) VALUES (10, 1, 'Old message', 0)",
                []
            ).unwrap();
            conn.execute(
                "INSERT INTO message (ROWID, handle_id, text, is_from_me) VALUES (20, 1, 'New message', 0)",
                []
            ).unwrap();
        }

        // Fetch only messages after ROWID 15
        let result = fetch_new_messages(&db_path, 15).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, 20);
        assert_eq!(result[0].2, "New message");
    }

    #[tokio::test]
    async fn fetch_new_messages_excludes_sent_messages() {
        let (_dir, db_path) = create_test_db();

        // Insert test data
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute(
                "INSERT INTO handle (ROWID, id) VALUES (1, '+1234567890')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO message (ROWID, handle_id, text, is_from_me) VALUES (10, 1, 'Received', 0)",
                []
            ).unwrap();
            conn.execute(
                "INSERT INTO message (ROWID, handle_id, text, is_from_me) VALUES (20, 1, 'Sent by me', 1)",
                []
            ).unwrap();
        }

        let result = fetch_new_messages(&db_path, 0).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].2, "Received");
    }

    #[tokio::test]
    async fn fetch_new_messages_excludes_null_text_and_null_body() {
        let (_dir, db_path) = create_test_db();

        // Insert test data: one with text, one with neither text nor attributedBody
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute(
                "INSERT INTO handle (ROWID, id) VALUES (1, '+1234567890')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO message (ROWID, handle_id, text, is_from_me) VALUES (10, 1, 'Has text', 0)",
                []
            ).unwrap();
            conn.execute(
                "INSERT INTO message (ROWID, handle_id, text, attributedBody, is_from_me) VALUES (20, 1, NULL, NULL, 0)",
                [],
            )
            .unwrap();
        }

        let result = fetch_new_messages(&db_path, 0).await.unwrap();
        // Message with NULL text AND NULL attributedBody is excluded
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].2, "Has text");
    }

    #[tokio::test]
    async fn fetch_new_messages_respects_limit() {
        let (_dir, db_path) = create_test_db();

        // Insert 25 messages (limit is 20)
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute(
                "INSERT INTO handle (ROWID, id) VALUES (1, '+1234567890')",
                [],
            )
            .unwrap();
            for i in 1..=25 {
                conn.execute(
                    &format!("INSERT INTO message (ROWID, handle_id, text, is_from_me) VALUES ({i}, 1, 'Message {i}', 0)"),
                    []
                ).unwrap();
            }
        }

        let result = fetch_new_messages(&db_path, 0).await.unwrap();
        assert_eq!(result.len(), 20); // Limited to 20
        assert_eq!(result[0].0, 1); // First message
        assert_eq!(result[19].0, 20); // 20th message
    }

    #[tokio::test]
    async fn fetch_new_messages_ordered_by_rowid_asc() {
        let (_dir, db_path) = create_test_db();

        // Insert messages out of order
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute(
                "INSERT INTO handle (ROWID, id) VALUES (1, '+1234567890')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO message (ROWID, handle_id, text, is_from_me) VALUES (30, 1, 'Third', 0)",
                []
            ).unwrap();
            conn.execute(
                "INSERT INTO message (ROWID, handle_id, text, is_from_me) VALUES (10, 1, 'First', 0)",
                []
            ).unwrap();
            conn.execute(
                "INSERT INTO message (ROWID, handle_id, text, is_from_me) VALUES (20, 1, 'Second', 0)",
                []
            ).unwrap();
        }

        let result = fetch_new_messages(&db_path, 0).await.unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].0, 10);
        assert_eq!(result[1].0, 20);
        assert_eq!(result[2].0, 30);
    }

    #[tokio::test]
    async fn fetch_new_messages_nonexistent_database() {
        let path = std::path::Path::new("/nonexistent/path/chat.db");
        let result = fetch_new_messages(path, 0).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn fetch_new_messages_handles_special_characters() {
        let (_dir, db_path) = create_test_db();

        // Insert message with special characters (potential SQL injection patterns)
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute(
                "INSERT INTO handle (ROWID, id) VALUES (1, '+1234567890')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO message (ROWID, handle_id, text, is_from_me) VALUES (10, 1, 'Hello \"world'' OR 1=1; DROP TABLE message;--', 0)",
                []
            ).unwrap();
        }

        let result = fetch_new_messages(&db_path, 0).await.unwrap();
        assert_eq!(result.len(), 1);
        // The special characters should be preserved, not interpreted as SQL
        assert!(result[0].2.contains("DROP TABLE"));
    }

    #[tokio::test]
    async fn fetch_new_messages_handles_unicode() {
        let (_dir, db_path) = create_test_db();

        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute(
                "INSERT INTO handle (ROWID, id) VALUES (1, '+1234567890')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO message (ROWID, handle_id, text, is_from_me) VALUES (10, 1, 'Hello 🦀 世界 مرحبا', 0)",
                []
            ).unwrap();
        }

        let result = fetch_new_messages(&db_path, 0).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].2, "Hello 🦀 世界 مرحبا");
    }

    #[tokio::test]
    async fn fetch_new_messages_filters_empty_text() {
        let (_dir, db_path) = create_test_db();

        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute(
                "INSERT INTO handle (ROWID, id) VALUES (1, '+1234567890')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO message (ROWID, handle_id, text, is_from_me) VALUES (10, 1, '', 0)",
                [],
            )
            .unwrap();
        }

        let result = fetch_new_messages(&db_path, 0).await.unwrap();
        // Empty-content messages are filtered out
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn fetch_new_messages_negative_rowid_edge_case() {
        let (_dir, db_path) = create_test_db();

        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute(
                "INSERT INTO handle (ROWID, id) VALUES (1, '+1234567890')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO message (ROWID, handle_id, text, is_from_me) VALUES (10, 1, 'Test', 0)",
                []
            ).unwrap();
        }

        // Negative rowid should still work (fetch all messages with ROWID > -1)
        let result = fetch_new_messages(&db_path, -1).await.unwrap();
        assert_eq!(result.len(), 1);
    }

    #[tokio::test]
    async fn fetch_new_messages_large_rowid_edge_case() {
        let (_dir, db_path) = create_test_db();

        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute(
                "INSERT INTO handle (ROWID, id) VALUES (1, '+1234567890')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO message (ROWID, handle_id, text, is_from_me) VALUES (10, 1, 'Test', 0)",
                []
            ).unwrap();
        }

        // Very large rowid should return empty (no messages after this)
        let result = fetch_new_messages(&db_path, i64::MAX - 1).await.unwrap();
        assert!(result.is_empty());
    }

    // ══════════════════════════════════════════════════════════
    // attributedBody / typedstream parsing tests
    // ══════════════════════════════════════════════════════════

    /// Build a minimal typedstream blob containing the given text.
    /// Format: [header] [class bytes] [0x01, 0x2B] [length-prefix] [utf8] [0x86, 0x84]
    fn make_attributed_body(text: &str) -> Vec<u8> {
        let text_bytes = text.as_bytes();
        let mut blob = Vec::new();
        // Fake streamtyped header (not parsed by our extractor)
        blob.extend_from_slice(b"\x04\x0bstreamtyped\x81\xe8\x03");
        // Class hierarchy bytes (skipped by marker scan)
        blob.extend_from_slice(b"\x84\x84NSMutableAttributedString\x00");
        // Start-of-text marker
        blob.push(0x01);
        blob.push(0x2B);
        // Length prefix (try_from panics on violation — correct for test helper)
        let len = text_bytes.len();
        if len <= 0x7F {
            blob.push(u8::try_from(len).unwrap());
        } else if len <= 0xFFFF {
            blob.push(0x81);
            blob.extend_from_slice(&u16::try_from(len).unwrap().to_le_bytes());
        } else {
            blob.push(0x82);
            blob.extend_from_slice(&u32::try_from(len).unwrap().to_le_bytes());
        }
        // Text content
        blob.extend_from_slice(text_bytes);
        // End-of-text marker
        blob.push(0x86);
        blob.push(0x84);
        // Trailing attribute bytes (ignored)
        blob.extend_from_slice(b"\x86\x86");
        blob
    }

    // Real attributedBody blob from macOS chat.db, captured during testing.
    // Decodes to: "Testing with imsg installed"
    const REAL_BLOB_TESTING: &[u8] = &[
        0x04, 0x0B, 0x73, 0x74, 0x72, 0x65, 0x61, 0x6D, 0x74, 0x79, 0x70, 0x65, 0x64, 0x81, 0xE8,
        0x03, 0x84, 0x01, 0x40, 0x84, 0x84, 0x84, 0x12, 0x4E, 0x53, 0x41, 0x74, 0x74, 0x72, 0x69,
        0x62, 0x75, 0x74, 0x65, 0x64, 0x53, 0x74, 0x72, 0x69, 0x6E, 0x67, 0x00, 0x84, 0x84, 0x08,
        0x4E, 0x53, 0x4F, 0x62, 0x6A, 0x65, 0x63, 0x74, 0x00, 0x85, 0x92, 0x84, 0x84, 0x84, 0x08,
        0x4E, 0x53, 0x53, 0x74, 0x72, 0x69, 0x6E, 0x67, 0x01, 0x94, 0x84, 0x01, 0x2B, 0x1B, 0x54,
        0x65, 0x73, 0x74, 0x69, 0x6E, 0x67, 0x20, 0x77, 0x69, 0x74, 0x68, 0x20, 0x69, 0x6D, 0x73,
        0x67, 0x20, 0x69, 0x6E, 0x73, 0x74, 0x61, 0x6C, 0x6C, 0x65, 0x64, 0x86, 0x84, 0x02, 0x69,
        0x49, 0x01, 0x1B, 0x92, 0x84, 0x84, 0x84, 0x0C, 0x4E, 0x53, 0x44, 0x69, 0x63, 0x74, 0x69,
        0x6F, 0x6E, 0x61, 0x72, 0x79, 0x00, 0x94, 0x84, 0x01, 0x69, 0x01, 0x92, 0x84, 0x96, 0x96,
        0x1D, 0x5F, 0x5F, 0x6B, 0x49, 0x4D, 0x4D, 0x65, 0x73, 0x73, 0x61, 0x67, 0x65, 0x50, 0x61,
        0x72, 0x74, 0x41, 0x74, 0x74, 0x72, 0x69, 0x62, 0x75, 0x74, 0x65, 0x4E, 0x61, 0x6D, 0x65,
        0x86, 0x92, 0x84, 0x84, 0x84, 0x08, 0x4E, 0x53, 0x4E, 0x75, 0x6D, 0x62, 0x65, 0x72, 0x00,
        0x84, 0x84, 0x07, 0x4E, 0x53, 0x56, 0x61, 0x6C, 0x75, 0x65, 0x00, 0x94, 0x84, 0x01, 0x2A,
        0x84, 0x99, 0x99, 0x00, 0x86, 0x86, 0x86,
    ];

    // Real attributedBody blob from unknownbreaker/MessageBridge (MIT).
    // Decodes to: "1"
    const REAL_BLOB_ONE: &[u8] = &[
        0x04, 0x0b, 0x73, 0x74, 0x72, 0x65, 0x61, 0x6d, 0x74, 0x79, 0x70, 0x65, 0x64, 0x81, 0xe8,
        0x03, 0x84, 0x01, 0x40, 0x84, 0x84, 0x84, 0x12, 0x4e, 0x53, 0x41, 0x74, 0x74, 0x72, 0x69,
        0x62, 0x75, 0x74, 0x65, 0x64, 0x53, 0x74, 0x72, 0x69, 0x6e, 0x67, 0x00, 0x84, 0x84, 0x08,
        0x4e, 0x53, 0x4f, 0x62, 0x6a, 0x65, 0x63, 0x74, 0x00, 0x85, 0x92, 0x84, 0x84, 0x84, 0x08,
        0x4e, 0x53, 0x53, 0x74, 0x72, 0x69, 0x6e, 0x67, 0x01, 0x94, 0x84, 0x01, 0x2b, 0x01, 0x31,
        0x86, 0x84, 0x02, 0x69, 0x49, 0x01, 0x01, 0x92, 0x84, 0x84, 0x84, 0x0c, 0x4e, 0x53, 0x44,
        0x69, 0x63, 0x74, 0x69, 0x6f, 0x6e, 0x61, 0x72, 0x79, 0x00, 0x94, 0x84, 0x01, 0x69, 0x01,
        0x92, 0x84, 0x96, 0x96, 0x1d, 0x5f, 0x5f, 0x6b, 0x49, 0x4d, 0x4d, 0x65, 0x73, 0x73, 0x61,
        0x67, 0x65, 0x50, 0x61, 0x72, 0x74, 0x41, 0x74, 0x74, 0x72, 0x69, 0x62, 0x75, 0x74, 0x65,
        0x4e, 0x61, 0x6d, 0x65, 0x86, 0x92, 0x84, 0x84, 0x84, 0x08, 0x4e, 0x53, 0x4e, 0x75, 0x6d,
        0x62, 0x65, 0x72, 0x00, 0x84, 0x84, 0x07, 0x4e, 0x53, 0x56, 0x61, 0x6c, 0x75, 0x65, 0x00,
        0x94, 0x84, 0x01, 0x2a, 0x84, 0x99, 0x99, 0x00, 0x86, 0x86, 0x86,
    ];

    #[test]
    fn extract_real_blob_testing_with_imsg() {
        let result = extract_text_from_attributed_body(REAL_BLOB_TESTING);
        assert_eq!(result, Some("Testing with imsg installed".to_string()));
    }

    #[test]
    fn extract_real_blob_single_char() {
        // From unknownbreaker/MessageBridge (MIT)
        let result = extract_text_from_attributed_body(REAL_BLOB_ONE);
        assert_eq!(result, Some("1".to_string()));
    }

    #[test]
    fn extract_text_containing_end_marker_bytes() {
        // U+2184 LATIN SMALL LETTER REVERSED C encodes to E2 86 84 in UTF-8.
        // The old parser scanned for [0x86, 0x84] as end marker and would
        // truncate here. The length-based parser must handle this correctly.
        let text = "before\u{2184}after";
        let blob = make_attributed_body(text);
        let result = extract_text_from_attributed_body(&blob);
        assert_eq!(result, Some(text.to_string()));
    }

    #[test]
    fn extract_zero_length_returns_empty_string() {
        // Marker found with length prefix = 0. Valid typedstream encoding
        // for an empty NSString — parser returns Some(""), which
        // resolve_message_content() will treat as empty and discard.
        let blob = b"\x01\x2B\x00";
        let result = extract_text_from_attributed_body(blob);
        assert_eq!(result, Some(String::new()));
    }

    #[test]
    fn extract_no_markers_returns_none() {
        let blob = b"just some random bytes with no markers";
        let result = extract_text_from_attributed_body(blob);
        assert!(result.is_none());
    }

    #[test]
    fn extract_invalid_utf8_returns_none() {
        let blob = b"\x01\x2B\x04\xFF\xFE\x80\x81";
        let result = extract_text_from_attributed_body(blob);
        assert!(result.is_none());
    }

    #[test]
    fn extract_truncated_blob_returns_none() {
        // Length prefix says 27 bytes but blob is truncated
        let blob = b"\x01\x2B\x1B\x54\x65\x73\x74";
        let result = extract_text_from_attributed_body(blob);
        assert!(result.is_none());
    }

    #[test]
    fn extract_long_text_two_byte_length() {
        // >127 bytes triggers 0x81 length prefix
        let long_text: String = "A".repeat(200);
        let blob = make_attributed_body(&long_text);
        let result = extract_text_from_attributed_body(&blob);
        assert_eq!(result, Some(long_text));
    }

    #[test]
    fn extract_four_byte_length_prefix() {
        // Test the 0x82 branch: 4-byte little-endian u32 length prefix.
        // Construct directly — make_attributed_body only emits 0x82 for >64KB.
        let text = b"Hello";
        let mut blob = Vec::new();
        blob.extend_from_slice(b"\x01\x2B"); // start marker
        blob.push(0x82); // 4-byte length tag
        blob.extend_from_slice(&5_u32.to_le_bytes()); // length = 5
        blob.extend_from_slice(text);
        let result = extract_text_from_attributed_body(&blob);
        assert_eq!(result, Some("Hello".to_string()));
    }

    #[test]
    fn extract_text_boundary_127_to_128() {
        // 127 is max single-byte length, 128 is min two-byte length
        for len in [127, 128] {
            let text: String = "X".repeat(len);
            let blob = make_attributed_body(&text);
            let result = extract_text_from_attributed_body(&blob);
            assert_eq!(result, Some(text), "failed at length {len}");
        }
    }

    #[tokio::test]
    async fn fetch_new_messages_reads_attributed_body_fallback() {
        let (_dir, db_path) = create_test_db();

        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute(
                "INSERT INTO handle (ROWID, id) VALUES (1, '+1234567890')",
                [],
            )
            .unwrap();
            // Real blob from macOS chat.db — text=NULL, attributedBody present
            conn.execute(
                "INSERT INTO message (ROWID, handle_id, text, attributedBody, is_from_me) VALUES (10, 1, NULL, ?1, 0)",
                [REAL_BLOB_TESTING.to_vec()],
            ).unwrap();
        }

        let result = fetch_new_messages(&db_path, 0).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].2, "Testing with imsg installed");
    }

    #[tokio::test]
    async fn fetch_new_messages_empty_text_falls_back_to_attributed_body() {
        let (_dir, db_path) = create_test_db();

        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute(
                "INSERT INTO handle (ROWID, id) VALUES (1, '+1234567890')",
                [],
            )
            .unwrap();
            // text = '' (empty string, not NULL) with valid attributedBody
            conn.execute(
                "INSERT INTO message (ROWID, handle_id, text, attributedBody, is_from_me) VALUES (10, 1, '', ?1, 0)",
                [REAL_BLOB_ONE.to_vec()],
            ).unwrap();
        }

        let result = fetch_new_messages(&db_path, 0).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].2, "1");
    }

    #[tokio::test]
    async fn fetch_new_messages_prefers_text_over_attributed_body() {
        let (_dir, db_path) = create_test_db();

        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute(
                "INSERT INTO handle (ROWID, id) VALUES (1, '+1234567890')",
                [],
            )
            .unwrap();
            // Both text and attributedBody present — text column wins
            conn.execute(
                "INSERT INTO message (ROWID, handle_id, text, attributedBody, is_from_me) VALUES (10, 1, 'Plain text', ?1, 0)",
                [REAL_BLOB_ONE.to_vec()],
            ).unwrap();
        }

        let result = fetch_new_messages(&db_path, 0).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].2, "Plain text");
    }

    #[tokio::test]
    async fn fetch_new_messages_mixed_text_and_attributed_body() {
        let (_dir, db_path) = create_test_db();

        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute(
                "INSERT INTO handle (ROWID, id) VALUES (1, '+1234567890')",
                [],
            )
            .unwrap();
            // Old-style message with text column
            conn.execute(
                "INSERT INTO message (ROWID, handle_id, text, is_from_me) VALUES (10, 1, 'Legacy message', 0)",
                []
            ).unwrap();
            // Modern message with only attributedBody (real blob)
            conn.execute(
                "INSERT INTO message (ROWID, handle_id, text, attributedBody, is_from_me) VALUES (20, 1, NULL, ?1, 0)",
                [REAL_BLOB_ONE.to_vec()],
            ).unwrap();
            // Message with neither (should be excluded)
            conn.execute(
                "INSERT INTO message (ROWID, handle_id, text, attributedBody, is_from_me) VALUES (30, 1, NULL, NULL, 0)",
                [],
            ).unwrap();
        }

        let result = fetch_new_messages(&db_path, 0).await.unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].2, "Legacy message");
        assert_eq!(result[1].2, "1");
    }
}
