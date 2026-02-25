use super::types::Commitment;

static COMMITMENT_PATTERNS: &[&str] = &[
    "I will ",
    "I'll ",
    "I am going to ",
    "I'm going to ",
    "Let me ",
];

pub fn extract_commitments(response: &str) -> Vec<Commitment> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut commitments = Vec::new();
    for line in response.lines() {
        let trimmed = line.trim();
        for pattern in COMMITMENT_PATTERNS {
            if trimmed.contains(pattern) {
                commitments.push(Commitment {
                    description: truncate(trimmed, 200),
                    made_at: now,
                    expires_at: Some(now + 3600),
                    fulfilled: false,
                    context: String::new(),
                });
                break;
            }
        }
    }
    commitments
}

pub fn check_fulfillment(commitments: &mut [Commitment], successful_tools: &[String]) {
    for c in commitments.iter_mut() {
        if c.fulfilled {
            continue;
        }
        let desc_lower = c.description.to_lowercase();
        for tool in successful_tools {
            if desc_lower.contains(&tool.to_lowercase()) {
                c.fulfilled = true;
                c.context = format!("fulfilled by tool: {}", tool);
                break;
            }
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let end = s
            .char_indices()
            .take_while(|(i, _)| *i < max)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(max);
        format!("{}...", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_commitments_from_response() {
        let response = "I will fix the bug.\nHere is some text.\nLet me check the logs.";
        let commits = extract_commitments(response);
        assert_eq!(commits.len(), 2);
        assert!(commits[0].description.contains("fix the bug"));
        assert!(commits[1].description.contains("check the logs"));
        assert!(!commits[0].fulfilled);
    }

    #[test]
    fn no_commitments_in_plain_text() {
        let response = "The fix is ready. All tests pass.";
        let commits = extract_commitments(response);
        assert!(commits.is_empty());
    }

    #[test]
    fn check_fulfillment_marks_matching() {
        let mut commitments = vec![Commitment {
            description: "I will use the shell tool".into(),
            made_at: 1000,
            expires_at: Some(2000),
            fulfilled: false,
            context: String::new(),
        }];
        check_fulfillment(&mut commitments, &["shell".into()]);
        assert!(commitments[0].fulfilled);
        assert!(commitments[0].context.contains("shell"));
    }

    #[test]
    fn already_fulfilled_skipped() {
        let mut commitments = vec![Commitment {
            description: "I will use shell".into(),
            made_at: 1000,
            expires_at: Some(2000),
            fulfilled: true,
            context: "already done".into(),
        }];
        check_fulfillment(&mut commitments, &["file_read".into()]);
        assert_eq!(commitments[0].context, "already done");
    }
}
