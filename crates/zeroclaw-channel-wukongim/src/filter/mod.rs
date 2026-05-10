// src/filter/mod.rs
use crate::connection::WkChannelType;

/// Returns true if `uid` is permitted by the allowlist.
/// An empty list denies everyone; a list containing `"*"` allows everyone.
pub fn is_user_allowed(allowed_users: &[String], uid: &str) -> bool {
    allowed_users.iter().any(|u| u == "*" || u == uid)
}

/// Parse a `recipient` string into `(channel_id, channel_type)`.
/// Format: `"<type>:<id>"` (e.g. `"2:group123"`) or bare `"<id>"` (personal).
pub fn parse_recipient(recipient: &str) -> (String, u8) {
    if let Some(pos) = recipient.find(':') {
        let (t_str, rest) = recipient.split_at(pos);
        let id = rest[1..].to_string();
        let t = t_str.parse::<u8>().unwrap_or(WkChannelType::PERSONAL);
        (id, t)
    } else {
        (recipient.to_string(), WkChannelType::PERSONAL)
    }
}

/// Returns true if the bot (`bot_uid`) is @-mentioned in this group message.
/// Checks the `mention` object in `payload_json` and falls back to text content scan.
pub fn is_mentioned(bot_uid: &str, payload_json: &serde_json::Value, content: &str) -> bool {
    if let Some(mention) = payload_json.get("mention") {
        if let Some(all) = mention.get("all") {
            let flagged = all.as_u64() == Some(1)
                || all.as_str() == Some("1")
                || all.as_str() == Some("true")
                || all.as_bool() == Some(true);
            if flagged {
                return true;
            }
        }
        if let Some(uids) = mention.get("uids").and_then(|v| v.as_array()) {
            if uids.iter().any(|u| {
                u.as_str() == Some(bot_uid)
                    || u.as_u64().map(|n| n.to_string()).as_deref() == Some(bot_uid)
            }) {
                return true;
            }
        }
    }
    content.contains(&format!("@{}", bot_uid)) || content.contains("@all")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_allows_everyone() {
        assert!(is_user_allowed(&["*".to_string()], "any-uid"));
    }

    #[test]
    fn specific_list_allows_only_listed() {
        let list = vec!["u1".to_string(), "u2".to_string()];
        assert!(is_user_allowed(&list, "u1"));
        assert!(!is_user_allowed(&list, "u3"));
    }

    #[test]
    fn empty_list_denies_all() {
        assert!(!is_user_allowed(&[], "anyone"));
    }

    #[test]
    fn parse_recipient_defaults_to_personal() {
        let (id, t) = parse_recipient("user123");
        assert_eq!(id, "user123");
        assert_eq!(t, 1u8);
    }

    #[test]
    fn parse_recipient_group_prefix() {
        let (id, t) = parse_recipient("2:group456");
        assert_eq!(id, "group456");
        assert_eq!(t, 2u8);
    }

    #[test]
    fn mention_check_uid_in_uids_array() {
        let payload = serde_json::json!({
            "mention": { "uids": ["bot001"] }
        });
        assert!(is_mentioned("bot001", &payload, ""));
    }

    #[test]
    fn mention_check_all_flag() {
        let payload = serde_json::json!({ "mention": { "all": 1 } });
        assert!(is_mentioned("anybot", &payload, ""));
    }

    #[test]
    fn mention_check_at_in_text() {
        let payload = serde_json::json!({});
        assert!(is_mentioned("bot001", &payload, "@bot001 please help"));
    }

    #[test]
    fn mention_check_not_mentioned() {
        let payload = serde_json::json!({});
        assert!(!is_mentioned("bot001", &payload, "hello world"));
    }
}
