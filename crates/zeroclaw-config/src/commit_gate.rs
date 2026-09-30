//! The instant a config write takes effect.
//!
//! A config write prepares everything that can wait (serializing, writing
//! and syncing a temporary file, backing up the current file) before it
//! replaces the canonical file with one synchronous rename. A
//! [`ConfigCommitGate`] decides at that instant whether the replacement may
//! happen, so a writer's authority can be established where the write takes
//! effect rather than before the waits that precede it.

/// Decides whether a prepared config write may replace the canonical file.
pub trait ConfigCommitGate: Send + Sync {
    /// Called once the replacement is prepared, with the synchronous
    /// `replace`. An implementation that refuses returns an error without
    /// calling it. One that allows calls `replace` while still holding
    /// whatever orders its decision against a concurrent revocation, and
    /// returns its result.
    fn commit(&self, replace: &mut dyn FnMut() -> std::io::Result<()>) -> anyhow::Result<()>;
}

/// No decision at commit: the caller authorized the write in full before
/// preparing it, or the write is not made on behalf of a principal.
pub struct UngatedCommit;

impl ConfigCommitGate for UngatedCommit {
    fn commit(&self, replace: &mut dyn FnMut() -> std::io::Result<()>) -> anyhow::Result<()> {
        replace().map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Config;

    /// Records what the directory holds when it is consulted, then refuses
    /// or allows.
    struct ObservingGate {
        allow: bool,
        dir: std::path::PathBuf,
        seen: std::sync::Mutex<Vec<String>>,
    }

    impl ConfigCommitGate for ObservingGate {
        fn commit(&self, replace: &mut dyn FnMut() -> std::io::Result<()>) -> anyhow::Result<()> {
            let mut names: Vec<String> = std::fs::read_dir(&self.dir)
                .unwrap()
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            *self.seen.lock().unwrap() = names;
            if self.allow {
                replace().map_err(Into::into)
            } else {
                anyhow::bail!("refused by the test gate")
            }
        }
    }

    async fn saved_config(tmp: &tempfile::TempDir) -> Config {
        let mut config = Config {
            config_path: tmp.path().join("config.toml"),
            data_dir: tmp.path().join("data"),
            ..Default::default()
        };
        config.save().await.expect("seed the config file");
        config.memory.backend = "none".into();
        config.mark_dirty("memory.backend");
        config
    }

    #[tokio::test]
    async fn the_gate_decides_after_every_wait_and_a_refusal_writes_nothing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = saved_config(&tmp).await;
        let before = std::fs::read(tmp.path().join("config.toml")).unwrap();
        let gate = ObservingGate {
            allow: false,
            dir: tmp.path().to_path_buf(),
            seen: std::sync::Mutex::new(Vec::new()),
        };

        let error = config
            .save_dirty_gated(&gate)
            .await
            .expect_err("a refusing gate refuses the write");

        assert!(format!("{error:#}").contains("refused"), "{error:#}");
        // The gate ran with the replacement fully prepared: the synced
        // temporary file and the backup were already on disk.
        let seen = gate.seen.lock().unwrap().clone();
        assert!(
            seen.iter()
                .any(|name| name.starts_with(".config.toml.tmp-")),
            "{seen:?}"
        );
        assert!(
            seen.iter().any(|name| name == "config.toml.bak"),
            "{seen:?}"
        );
        // Refused: canonical bytes untouched, nothing left behind.
        assert_eq!(
            std::fs::read(tmp.path().join("config.toml")).unwrap(),
            before
        );
        let left: Vec<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "config.toml" && name != "data")
            .collect();
        assert!(left.is_empty(), "stray files after a refusal: {left:?}");
        assert!(!config.dirty_paths.is_empty(), "a refused save stays dirty");
    }

    #[tokio::test]
    async fn an_allowing_gate_replaces_the_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = saved_config(&tmp).await;
        let gate = ObservingGate {
            allow: true,
            dir: tmp.path().to_path_buf(),
            seen: std::sync::Mutex::new(Vec::new()),
        };

        config
            .save_dirty_gated(&gate)
            .await
            .expect("the gate allows it");

        let on_disk = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
        assert!(on_disk.contains("none"), "{on_disk}");
        assert!(config.dirty_paths.is_empty());
    }
}
