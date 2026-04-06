#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoneResult {
    Pr(String),
    File(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StageInfo {
    pub stage_cur: Option<u32>,
    pub stage_total: Option<u32>,
    pub stage_label: Option<String>,
    pub done: Option<DoneResult>,
    pub failed: Option<String>,
}

pub fn parse_stdout_log(text: &str) -> StageInfo {
    let mut info = StageInfo::default();

    for line in text.lines() {
        let trimmed = line.trim_start();

        if let Some(rest) = trimmed.strip_prefix("[FAIL:") {
            let reason = rest.trim_end_matches(']').trim();
            info.failed = Some(reason.to_owned());
        } else if let Some(rest) = trimmed.strip_prefix("[DONE:") {
            let inner = rest.trim_end_matches(']').trim();
            if let Some(url) = inner.strip_prefix("PR=") {
                info.done = Some(DoneResult::Pr(url.to_owned()));
            } else if let Some(file) = inner.strip_prefix("FILE=") {
                info.done = Some(DoneResult::File(file.to_owned()));
            }
        } else if trimmed.starts_with("[STAGE ") {
            // Only match when [STAGE is at the true start of the line
            // (leading chars, if any, must all be whitespace)
            let leading = &line[..line.len() - trimmed.len()];
            if leading.is_empty() || leading.trim().is_empty() {
                if let Some(parsed) = parse_stage_marker(trimmed) {
                    info.stage_cur = Some(parsed.0);
                    info.stage_total = Some(parsed.1);
                    info.stage_label = Some(parsed.2);
                }
            }
        }
    }

    info
}

fn parse_stage_marker(s: &str) -> Option<(u32, u32, String)> {
    // Expected: "[STAGE N/M] label"
    let rest = s.strip_prefix("[STAGE ")?;
    let bracket_end = rest.find(']')?;
    let inside = &rest[..bracket_end];
    let slash = inside.find('/')?;
    let n: u32 = inside[..slash].parse().ok()?;
    let m: u32 = inside[slash + 1..].parse().ok()?;
    let label = rest[bracket_end + 1..].trim().to_owned();
    Some((n, m, label))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_stage_marker() {
        let info = parse_stdout_log("[STAGE 2/5] Planning");
        assert_eq!(info.stage_cur, Some(2));
        assert_eq!(info.stage_total, Some(5));
        assert_eq!(info.stage_label.as_deref(), Some("Planning"));
        assert!(info.done.is_none());
        assert!(info.failed.is_none());
    }

    #[test]
    fn multiple_stage_markers_last_wins() {
        let text =
            "[STAGE 1/3] Init\nsome output\n[STAGE 2/3] Build\nmore output\n[STAGE 3/3] Deploy";
        let info = parse_stdout_log(text);
        assert_eq!(info.stage_cur, Some(3));
        assert_eq!(info.stage_total, Some(3));
        assert_eq!(info.stage_label.as_deref(), Some("Deploy"));
    }

    #[test]
    fn done_pr_marker() {
        let text = "[STAGE 1/2] Work\n[DONE: PR=https://github.com/org/repo/pull/42]";
        let info = parse_stdout_log(text);
        assert_eq!(
            info.done,
            Some(DoneResult::Pr("https://github.com/org/repo/pull/42".into()))
        );
    }

    #[test]
    fn done_file_marker() {
        let text = "[DONE: FILE=report.png]";
        let info = parse_stdout_log(text);
        assert_eq!(info.done, Some(DoneResult::File("report.png".into())));
    }

    #[test]
    fn fail_marker() {
        let text = "[STAGE 1/3] Build\ncargo output...\n[FAIL: cargo error]";
        let info = parse_stdout_log(text);
        assert_eq!(info.failed.as_deref(), Some("cargo error"));
    }

    #[test]
    fn no_markers_all_none() {
        let text = "just some random log output\nno markers here\n";
        let info = parse_stdout_log(text);
        assert_eq!(info, StageInfo::default());
    }

    #[test]
    fn stage_not_at_line_start_ignored() {
        let text = "prefix [STAGE 2/5] Planning";
        let info = parse_stdout_log(text);
        assert!(info.stage_cur.is_none());
        assert!(info.stage_total.is_none());
        assert!(info.stage_label.is_none());
    }
}
