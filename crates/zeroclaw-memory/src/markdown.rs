use super::traits::{Memory, MemoryCategory, MemoryEntry, is_recent_recall_query};
use async_trait::async_trait;
use chrono::{DateTime, FixedOffset, Local, NaiveDate};
use std::path::{Path, PathBuf};
use tokio::fs;

fn entry_in_window(
    timestamp: &str,
    since: Option<&DateTime<FixedOffset>>,
    until: Option<&DateTime<FixedOffset>>,
) -> bool {
    if let Ok(ts) = DateTime::parse_from_rfc3339(timestamp) {
        if let Some(s) = since
            && ts < *s
        {
            return false;
        }
        if let Some(u) = until
            && ts > *u
        {
            return false;
        }
        return true;
    }
    if let Ok(date) = NaiveDate::parse_from_str(timestamp, "%Y-%m-%d") {
        if let Some(s) = since
            && date < s.date_naive()
        {
            return false;
        }
        if let Some(u) = until
            && date > u.date_naive()
        {
            return false;
        }
        return true;
    }
    // Non-date stems (e.g. MEMORY.md) are evergreen; never window-filtered.
    true
}

pub struct MarkdownMemory {
    alias: String,
    workspace_dir: PathBuf,
}

impl MarkdownMemory {
    pub fn new(alias: &str, workspace_dir: &Path) -> Self {
        Self {
            alias: alias.to_string(),
            workspace_dir: workspace_dir.to_path_buf(),
        }
    }

    fn memory_dir(&self) -> PathBuf {
        self.workspace_dir.join("memory")
    }

    fn core_path(&self) -> PathBuf {
        self.workspace_dir.join("MEMORY.md")
    }

    fn daily_path(&self) -> PathBuf {
        let date = Local::now().format("%Y-%m-%d").to_string();
        self.memory_dir().join(format!("{date}.md"))
    }

    async fn ensure_dirs(&self) -> anyhow::Result<()> {
        fs::create_dir_all(self.memory_dir()).await?;
        Ok(())
    }

    async fn append_to_file(&self, path: &Path, content: &str) -> anyhow::Result<()> {
        self.ensure_dirs().await?;

        let row = format!("\n{content}\n");
        let header = if path == self.core_path() {
            "# Long-Term Memory\n\n".to_string()
        } else {
            let date = Local::now().format("%Y-%m-%d").to_string();
            format!("# Daily Log — {date}\n\n")
        };
        let first_write = format!("{header}{content}\n");
        let path = path.to_path_buf();

        // Never read-modify-write: another handle or process can add a row, or
        // be mid-truncate, between our read and our write. One `write_all` on an
        // append handle lets the kernel place the row at the current end.
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            use std::fs::OpenOptions;
            use std::io::Write;

            match OpenOptions::new().append(true).open(&path) {
                Ok(mut file) => return file.write_all(row.as_bytes()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            // `create_new` so exactly one creator wins and the file gets created
            // with content on this attempt; the loser falls back to append below.
            // Known limitation: the winner's `create_new` and its `write_all` are
            // two separate syscalls, so a loser can append its row in between and
            // land before the header. Cosmetic only — `parse_entries_from_file`
            // below drops every line starting with `#` regardless of position, so
            // no row is lost and no entry is misread; see PR description.
            match OpenOptions::new().create_new(true).append(true).open(&path) {
                Ok(mut file) => file.write_all(first_write.as_bytes()),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => OpenOptions::new()
                    .append(true)
                    .open(&path)?
                    .write_all(row.as_bytes()),
                Err(e) => Err(e),
            }
        })
        .await??;
        Ok(())
    }

    fn parse_entries_from_file(
        path: &Path,
        content: &str,
        category: &MemoryCategory,
    ) -> Vec<MemoryEntry> {
        let filename = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");

        content
            .lines()
            .filter(|line| {
                let trimmed = line.trim();
                !trimmed.is_empty() && !trimmed.starts_with('#')
            })
            .enumerate()
            .map(|(i, line)| {
                let trimmed = line.trim();
                let clean = trimmed.strip_prefix("- ").unwrap_or(trimmed);
                MemoryEntry {
                    principal_id: None,
                    id: format!("{filename}:{i}"),
                    key: format!("{filename}:{i}"),
                    content: clean.to_string(),
                    category: category.clone(),
                    timestamp: filename.to_string(),
                    session_id: None,
                    score: None,
                    namespace: "default".into(),
                    importance: None,
                    superseded_by: None,
                    kind: None,
                    pinned: false,
                    tenant_id: None,
                    agent_alias: None,
                    agent_id: None,
                }
            })
            .collect()
    }

    async fn read_all_entries(&self) -> anyhow::Result<Vec<MemoryEntry>> {
        let mut entries = Vec::new();

        // Read MEMORY.md (core)
        let core_path = self.core_path();
        if core_path.exists() {
            let content = fs::read_to_string(&core_path).await?;
            entries.extend(Self::parse_entries_from_file(
                &core_path,
                &content,
                &MemoryCategory::Core,
            ));
        }

        // Read daily logs
        let mem_dir = self.memory_dir();
        if mem_dir.exists() {
            let mut dir = fs::read_dir(&mem_dir).await?;
            while let Some(entry) = dir.next_entry().await? {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("md") {
                    let content = fs::read_to_string(&path).await?;
                    entries.extend(Self::parse_entries_from_file(
                        &path,
                        &content,
                        &MemoryCategory::Daily,
                    ));
                }
            }
        }

        entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        Ok(entries)
    }
}

#[async_trait]
impl Memory for MarkdownMemory {
    fn name(&self) -> &str {
        "markdown"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        _session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let entry = format!("- **{key}**: {content}");
        let path = match category {
            MemoryCategory::Core => self.core_path(),
            _ => self.daily_path(),
        };
        self.append_to_file(&path, &entry).await
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        _session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let since_dt = since
            .map(chrono::DateTime::parse_from_rfc3339)
            .transpose()
            .map_err(|e| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(
                            ::serde_json::json!({"field": "since", "error": format!("{}", e)})
                        ),
                    "recall window bound rejected"
                );
                anyhow::Error::msg(format!("invalid 'since' date (expected RFC 3339): {e}"))
            })?;
        let until_dt = until
            .map(chrono::DateTime::parse_from_rfc3339)
            .transpose()
            .map_err(|e| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(
                            ::serde_json::json!({"field": "until", "error": format!("{}", e)})
                        ),
                    "recall window bound rejected"
                );
                anyhow::Error::msg(format!("invalid 'until' date (expected RFC 3339): {e}"))
            })?;
        if let (Some(s), Some(u)) = (&since_dt, &until_dt)
            && s >= u
        {
            anyhow::bail!("'since' must be before 'until'");
        }

        let all = self.read_all_entries().await?;
        let keywords: Vec<String> = if is_recent_recall_query(query) {
            Vec::new()
        } else {
            query
                .to_lowercase()
                .split_whitespace()
                .map(str::to_string)
                .collect()
        };

        let mut scored: Vec<MemoryEntry> = all
            .into_iter()
            .filter_map(|mut entry| {
                if !entry_in_window(&entry.timestamp, since_dt.as_ref(), until_dt.as_ref()) {
                    return None;
                }
                if keywords.is_empty() {
                    entry.score = Some(1.0);
                    return Some(entry);
                }
                let content_lower = entry.content.to_lowercase();
                let matched = keywords
                    .iter()
                    .filter(|kw| content_lower.contains(kw.as_str()))
                    .count();
                if matched > 0 {
                    #[allow(clippy::cast_precision_loss)]
                    let score = matched as f64 / keywords.len() as f64;
                    entry.score = Some(score);
                    Some(entry)
                } else {
                    None
                }
            })
            .collect();

        scored.sort_by(|a, b| {
            if keywords.is_empty() {
                b.timestamp.as_str().cmp(a.timestamp.as_str())
            } else {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }
        });
        scored.truncate(limit);
        Ok(scored)
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        let all = self.read_all_entries().await?;
        Ok(all
            .into_iter()
            .find(|e| e.key == key || e.content.contains(key)))
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        _session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let all = self.read_all_entries().await?;
        match category {
            Some(cat) => Ok(all.into_iter().filter(|e| &e.category == cat).collect()),
            None => Ok(all),
        }
    }

    async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
        // Markdown memory is append-only by design (audit trail)
        // Return false to indicate the entry wasn't removed
        Ok(false)
    }

    async fn forget_for_agent(&self, _key: &str, _agent_id: &str) -> anyhow::Result<bool> {
        Ok(false)
    }

    async fn count(&self) -> anyhow::Result<usize> {
        let all = self.read_all_entries().await?;
        Ok(all.len())
    }

    async fn health_check(&self) -> bool {
        self.workspace_dir.exists()
    }

    async fn store_with_agent(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        _namespace: Option<&str>,
        _importance: Option<f64>,
        _agent_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.store(key, content, category, session_id).await
    }

    async fn recall_for_agents(
        &self,
        _allowed_agent_ids: &[&str],
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.recall(query, limit, session_id, since, until).await
    }
}

impl ::zeroclaw_api::attribution::Attributable for MarkdownMemory {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Memory(::zeroclaw_api::attribution::MemoryKind::Markdown)
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::TempDir;

    fn temp_workspace() -> (TempDir, MarkdownMemory) {
        let tmp = TempDir::new().unwrap();
        let mem = MarkdownMemory::new("markdown", tmp.path());
        (tmp, mem)
    }

    #[tokio::test]
    async fn markdown_name() {
        let (_tmp, mem) = temp_workspace();
        assert_eq!(mem.name(), "markdown");
    }

    #[tokio::test]
    async fn markdown_health_check() {
        let (_tmp, mem) = temp_workspace();
        assert!(mem.health_check().await);
    }

    #[tokio::test]
    async fn markdown_store_core() {
        let (_tmp, mem) = temp_workspace();
        mem.store("pref", "User likes Rust", MemoryCategory::Core, None)
            .await
            .unwrap();
        let content = fs::read_to_string(mem.core_path()).await.unwrap();
        assert!(content.contains("User likes Rust"));
    }

    #[tokio::test]
    async fn markdown_store_daily() {
        let (_tmp, mem) = temp_workspace();
        mem.store("note", "Finished tests", MemoryCategory::Daily, None)
            .await
            .unwrap();
        let path = mem.daily_path();
        let content = fs::read_to_string(path).await.unwrap();
        assert!(content.contains("Finished tests"));
    }

    #[tokio::test]
    async fn markdown_recall_keyword() {
        let (_tmp, mem) = temp_workspace();
        mem.store("a", "Rust is fast", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("b", "Python is slow", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("c", "Rust and safety", MemoryCategory::Core, None)
            .await
            .unwrap();

        let results = mem.recall("Rust", 10, None, None, None).await.unwrap();
        assert!(results.len() >= 2);
        assert!(
            results
                .iter()
                .all(|r| r.content.to_lowercase().contains("rust"))
        );
    }

    #[tokio::test]
    async fn markdown_recall_no_match() {
        let (_tmp, mem) = temp_workspace();
        mem.store("a", "Rust is great", MemoryCategory::Core, None)
            .await
            .unwrap();
        let results = mem
            .recall("javascript", 10, None, None, None)
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn markdown_recall_star_query_returns_recent_entries() {
        let (_tmp, mem) = temp_workspace();
        mem.store("a", "first memory", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("b", "second memory", MemoryCategory::Daily, None)
            .await
            .unwrap();

        let results = mem.recall("*", 10, None, None, None).await.unwrap();
        assert_eq!(results.len(), 2);
        assert!(
            results
                .iter()
                .any(|entry| entry.content.contains("first memory"))
        );
        assert!(
            results
                .iter()
                .any(|entry| entry.content.contains("second memory"))
        );
    }

    #[tokio::test]
    async fn markdown_count() {
        let (_tmp, mem) = temp_workspace();
        mem.store("a", "first", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("b", "second", MemoryCategory::Core, None)
            .await
            .unwrap();
        let count = mem.count().await.unwrap();
        assert!(count >= 2);
    }

    #[tokio::test]
    async fn markdown_list_by_category() {
        let (_tmp, mem) = temp_workspace();
        mem.store("a", "core fact", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("b", "daily note", MemoryCategory::Daily, None)
            .await
            .unwrap();

        let core = mem.list(Some(&MemoryCategory::Core), None).await.unwrap();
        assert!(core.iter().all(|e| e.category == MemoryCategory::Core));

        let daily = mem.list(Some(&MemoryCategory::Daily), None).await.unwrap();
        assert!(daily.iter().all(|e| e.category == MemoryCategory::Daily));
    }

    #[tokio::test]
    async fn markdown_forget_is_noop() {
        let (_tmp, mem) = temp_workspace();
        mem.store("a", "permanent", MemoryCategory::Core, None)
            .await
            .unwrap();
        let removed = mem.forget("a").await.unwrap();
        assert!(!removed, "Markdown memory is append-only");
    }

    #[tokio::test]
    async fn markdown_empty_recall() {
        let (_tmp, mem) = temp_workspace();
        let results = mem.recall("anything", 10, None, None, None).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn markdown_empty_count() {
        let (_tmp, mem) = temp_workspace();
        assert_eq!(mem.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn markdown_entries_carry_no_agent_attribution() {
        let (_tmp, mem) = temp_workspace();
        mem.store("k", "v", MemoryCategory::Core, None)
            .await
            .unwrap();
        let entry = mem.get("MEMORY.md:0").await.unwrap();
        if let Some(entry) = entry {
            assert!(
                entry.agent_alias.is_none(),
                "markdown rows must never claim an agent alias"
            );
            assert!(
                entry.agent_id.is_none(),
                "markdown rows must never claim a raw agent id either"
            );
        }
        // list path must show the same shape regardless of how a row is
        // surfaced (keyed lookup vs. enumeration).
        let rows = mem.list(None, None).await.unwrap();
        for row in rows {
            assert!(
                row.agent_alias.is_none(),
                "list path must not synthesize aliases"
            );
            assert!(row.agent_id.is_none(), "list path must not synthesize ids");
        }
    }

    #[tokio::test]
    async fn markdown_recall_since_until_filters_daily() {
        let (_tmp, mem) = temp_workspace();
        mem.store("today", "daily standup note", MemoryCategory::Daily, None)
            .await
            .unwrap();
        mem.store("core", "evergreen daily fact", MemoryCategory::Core, None)
            .await
            .unwrap();

        let today = Local::now().date_naive();
        let yesterday = (today - chrono::Duration::days(1))
            .and_hms_opt(23, 59, 59)
            .unwrap();
        let yesterday_rfc = Local.from_local_datetime(&yesterday).unwrap().to_rfc3339();
        let past = (today - chrono::Duration::days(7))
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let past_rfc = Local.from_local_datetime(&past).unwrap().to_rfc3339();

        // until = yesterday: today's daily entry is outside the window and
        // must be dropped, but the evergreen MEMORY.md entry must survive.
        let bounded = mem
            .recall("daily", 10, None, None, Some(&yesterday_rfc))
            .await
            .unwrap();
        assert!(
            !bounded.iter().any(|e| e.content.contains("standup")),
            "today's daily entry must be excluded when until=yesterday"
        );
        assert!(
            bounded.iter().any(|e| e.content.contains("evergreen")),
            "evergreen MEMORY.md entry must not be window-filtered"
        );

        // since = a week ago: today's daily entry is inside the window.
        let recent = mem
            .recall("daily", 10, None, Some(&past_rfc), None)
            .await
            .unwrap();
        assert!(
            recent.iter().any(|e| e.content.contains("standup")),
            "today's daily entry must be included when since is in the past"
        );
    }

    #[tokio::test]
    async fn store_keeps_the_established_file_format() {
        let (tmp, mem) = temp_workspace();
        mem.store("a", "1", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("b", "2", MemoryCategory::Core, None)
            .await
            .unwrap();

        let raw = std::fs::read_to_string(tmp.path().join("MEMORY.md")).unwrap();
        assert_eq!(raw, "# Long-Term Memory\n\n- **a**: 1\n\n- **b**: 2\n");
    }

    #[tokio::test]
    async fn store_keeps_prior_bytes_when_existing_file_is_not_valid_utf8() {
        let (tmp, mem) = temp_workspace();
        let path = tmp.path().join("MEMORY.md");
        std::fs::write(&path, [b'#', b' ', 0xFF, 0xFE, b'\n']).unwrap();

        mem.store("new", "newly stored", MemoryCategory::Core, None)
            .await
            .unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert!(
            bytes.starts_with(&[b'#', b' ', 0xFF, 0xFE, b'\n']),
            "prior bytes must survive a store"
        );
        assert!(String::from_utf8_lossy(&bytes).contains("- **new**: newly stored"));
    }

    // Append never reads, so it cannot tell "existing but empty" from
    // "missing"; deciding whether a header is owed would require reading.
    #[tokio::test]
    async fn store_adds_no_header_to_an_existing_but_empty_file() {
        let (tmp, mem) = temp_workspace();
        let path = tmp.path().join("MEMORY.md");
        std::fs::write(&path, b"").unwrap();

        mem.store("new", "newly stored", MemoryCategory::Core, None)
            .await
            .unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            !raw.contains("# Long-Term Memory"),
            "an existing empty file must not retroactively gain a header: {raw:?}"
        );
        assert!(raw.contains("- **new**: newly stored"));
    }

    // `create_new` refuses a path that already exists as a symlink, dangling
    // or not, so a dangling MEMORY.md symlink must now fail instead of
    // silently creating a file at the link's target.
    #[cfg(unix)]
    #[tokio::test]
    async fn store_fails_on_a_dangling_symlink_instead_of_creating_its_target() {
        let (tmp, mem) = temp_workspace();
        let target = tmp.path().join("nowhere.md");
        let link = tmp.path().join("MEMORY.md");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let err = mem
            .store("new", "newly stored", MemoryCategory::Core, None)
            .await
            .expect_err("a dangling symlink must not be silently created through");

        assert!(
            !target.exists(),
            "the link's target must not be created: {err}"
        );
    }

    // Each task builds its own `MarkdownMemory`, as the memory factory does per
    // call, so no per-instance lock can mask a lost update.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_stores_from_independent_instances_lose_no_row() {
        let tmp = TempDir::new().unwrap();
        for round in 0..20 {
            let dir = tmp.path().join(format!("r{round}"));
            std::fs::create_dir_all(&dir).unwrap();
            let mut seed = String::from("# Long-Term Memory\n\n");
            for p in 0..20 {
                seed.push_str(&format!("- **prev{p}**: old\n"));
            }
            std::fs::write(dir.join("MEMORY.md"), seed).unwrap();

            let mut tasks = tokio::task::JoinSet::new();
            for i in 0..8 {
                let dir = dir.clone();
                tasks.spawn(async move {
                    MarkdownMemory::new("markdown", &dir)
                        .store(&format!("row{i}"), "v", MemoryCategory::Core, None)
                        .await
                });
            }
            while let Some(res) = tasks.join_next().await {
                res.unwrap().unwrap();
            }

            let raw = std::fs::read_to_string(dir.join("MEMORY.md")).unwrap();
            for p in 0..20 {
                assert!(
                    raw.contains(&format!("**prev{p}**")),
                    "round {round}: prev{p} lost"
                );
            }
            for i in 0..8 {
                assert!(
                    raw.contains(&format!("**row{i}**")),
                    "round {round}: row{i} lost"
                );
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_first_stores_write_one_header_and_every_row() {
        let tmp = TempDir::new().unwrap();
        for round in 0..20 {
            let dir = tmp.path().join(format!("n{round}"));
            std::fs::create_dir_all(&dir).unwrap();
            let mut tasks = tokio::task::JoinSet::new();
            for i in 0..16 {
                let dir = dir.clone();
                tasks.spawn(async move {
                    MarkdownMemory::new("markdown", &dir)
                        .store(&format!("row{i}"), "v", MemoryCategory::Core, None)
                        .await
                });
            }
            while let Some(res) = tasks.join_next().await {
                res.unwrap().unwrap();
            }

            let raw = std::fs::read_to_string(dir.join("MEMORY.md")).unwrap();
            assert_eq!(
                raw.matches("# Long-Term Memory").count(),
                1,
                "round {round}"
            );
            for i in 0..16 {
                assert!(
                    raw.contains(&format!("**row{i}**")),
                    "round {round}: row{i} lost"
                );
            }
        }
    }
}
