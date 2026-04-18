// Preset Loader (v3.0 Section E)
//
// Loads bundled workflow presets (YAML embedded via `include_str!`) and
// imports them into the `workflows` table on first run. Presets are immutable
// seeds — users can fork/customize via `parent_workflow_id`.

use anyhow::{Context, Result};
use rusqlite::params;
use sha2::{Digest, Sha256};

use super::parser::{parse_spec, WorkflowSpec};

/// A bundled preset workflow (YAML text + metadata).
#[derive(Debug, Clone)]
pub struct Preset {
    pub slug: &'static str,
    pub yaml: &'static str,
}

/// All bundled lawyer presets, embedded at compile time.
pub const LAWYER_PRESETS: &[Preset] = &[
    Preset {
        slug: "lawyer_01_new_client_intake",
        yaml: include_str!("presets/lawyer_01_new_client_intake.yaml"),
    },
    Preset {
        slug: "lawyer_02_consultation_journal",
        yaml: include_str!("presets/lawyer_02_consultation_journal.yaml"),
    },
    Preset {
        slug: "lawyer_03_case_precedent_summary",
        yaml: include_str!("presets/lawyer_03_case_precedent_summary.yaml"),
    },
    Preset {
        slug: "lawyer_04_complaint_draft",
        yaml: include_str!("presets/lawyer_04_complaint_draft.yaml"),
    },
    Preset {
        slug: "lawyer_05_contract_review",
        yaml: include_str!("presets/lawyer_05_contract_review.yaml"),
    },
    Preset {
        slug: "lawyer_06_hearing_briefing",
        yaml: include_str!("presets/lawyer_06_hearing_briefing.yaml"),
    },
    Preset {
        slug: "lawyer_07_weekly_report",
        yaml: include_str!("presets/lawyer_07_weekly_report.yaml"),
    },
    Preset {
        slug: "lawyer_08_court_interpret",
        yaml: include_str!("presets/lawyer_08_court_interpret.yaml"),
    },
    Preset {
        slug: "lawyer_09_case_timeline_update",
        yaml: include_str!("presets/lawyer_09_case_timeline_update.yaml"),
    },
    Preset {
        slug: "lawyer_10_receipt_expense",
        yaml: include_str!("presets/lawyer_10_receipt_expense.yaml"),
    },
];

/// All bundled shopping presets, embedded at compile time.
pub const SHOPPING_PRESETS: &[Preset] = &[
    Preset {
        slug: "shopping_01_product_search",
        yaml: include_str!("presets/shopping/01_product_search.yaml"),
    },
    Preset {
        slug: "shopping_02_seasonal_outfit",
        yaml: include_str!("presets/shopping/02_seasonal_outfit.yaml"),
    },
    Preset {
        slug: "shopping_03a_recurring_register",
        yaml: include_str!("presets/shopping/03a_recurring_register.yaml"),
    },
    Preset {
        slug: "shopping_03b_recurring_execute",
        yaml: include_str!("presets/shopping/03b_recurring_execute.yaml"),
    },
    Preset {
        slug: "shopping_04_one_time_purchase",
        yaml: include_str!("presets/shopping/04_one_time_purchase.yaml"),
    },
    Preset {
        slug: "shopping_05_gift_recommendation",
        yaml: include_str!("presets/shopping/05_gift_recommendation.yaml"),
    },
    Preset {
        slug: "shopping_06_open_run_ticket",
        yaml: include_str!("presets/shopping/06_open_run_ticket.yaml"),
    },
    Preset {
        slug: "shopping_07_price_drop_watch",
        yaml: include_str!("presets/shopping/07_price_drop_watch.yaml"),
    },
    Preset {
        slug: "shopping_08_coupon_max_discount",
        yaml: include_str!("presets/shopping/08_coupon_max_discount.yaml"),
    },
    Preset {
        slug: "shopping_09_return_refund",
        yaml: include_str!("presets/shopping/09_return_refund.yaml"),
    },
];

/// All bundled presets combined.
pub fn all_presets() -> Vec<&'static Preset> {
    let mut all: Vec<&'static Preset> = LAWYER_PRESETS.iter().collect();
    all.extend(SHOPPING_PRESETS.iter());
    all
}

/// Parse a preset into a WorkflowSpec.
pub fn parse_preset(preset: &Preset) -> Result<WorkflowSpec> {
    parse_spec(preset.yaml).with_context(|| format!("preset '{}' invalid", preset.slug))
}

/// Parse all presets, returning any that fail to parse with their error.
pub fn validate_all_presets(presets: &[Preset]) -> Vec<(String, String)> {
    let mut failures = Vec::new();
    for preset in presets {
        if let Err(e) = parse_preset(preset) {
            failures.push((preset.slug.to_string(), format!("{e:#}")));
        }
    }
    failures
}

/// Compute SHA256 of preset YAML content (for workflows.spec_sha256).
pub fn preset_sha256(preset: &Preset) -> String {
    let mut hasher = Sha256::new();
    hasher.update(preset.yaml.as_bytes());
    hex::encode(hasher.finalize())
}

/// Import presets into the workflows table (idempotent — skips existing slugs).
///
/// Returns the number of presets newly imported.
pub fn import_presets(
    conn: &rusqlite::Connection,
    presets: &[Preset],
) -> Result<usize> {
    let mut imported = 0;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    for preset in presets {
        let spec = parse_preset(preset)?;
        let sha = preset_sha256(preset);
        let uuid = format!("preset-{}", preset.slug);

        // Check if this preset UUID already exists
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM workflows WHERE uuid = ?1)",
                params![uuid],
                |r| r.get(0),
            )
            .unwrap_or(false);

        if exists {
            continue;
        }

        conn.execute(
            "INSERT INTO workflows
                (uuid, parent_category, name, description, icon, spec_yaml, spec_sha256,
                 trigger_type, trigger_config_json, version, created_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                uuid,
                spec.parent_category,
                spec.name,
                spec.description,
                spec.icon,
                preset.yaml,
                sha,
                "manual",
                None::<String>,
                1,
                "preset",
                now,
                now,
            ],
        )?;
        imported += 1;
    }

    Ok(imported)
}

/// Ensure the workflows table exists (for environments where S2 migration hasn't run yet).
pub fn ensure_workflows_schema(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS workflows (
            id                   INTEGER PRIMARY KEY AUTOINCREMENT,
            uuid                 TEXT NOT NULL UNIQUE,
            parent_category      TEXT NOT NULL,
            name                 TEXT NOT NULL,
            description          TEXT,
            icon                 TEXT,
            spec_yaml            TEXT NOT NULL,
            spec_sha256          TEXT NOT NULL,
            trigger_type         TEXT NOT NULL DEFAULT 'manual',
            trigger_config_json  TEXT,
            version              INTEGER NOT NULL DEFAULT 1,
            parent_workflow_id   INTEGER,
            created_by           TEXT NOT NULL DEFAULT 'user',
            usage_count          INTEGER NOT NULL DEFAULT 0,
            last_used_at         INTEGER,
            is_pinned            INTEGER NOT NULL DEFAULT 0,
            is_archived          INTEGER NOT NULL DEFAULT 0,
            created_at           INTEGER NOT NULL,
            updated_at           INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_wf_category
            ON workflows(parent_category, is_pinned DESC, usage_count DESC)
            WHERE is_archived = 0;",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn all_lawyer_presets_parse() {
        let failures = validate_all_presets(LAWYER_PRESETS);
        assert!(
            failures.is_empty(),
            "Some presets failed to parse: {failures:?}"
        );
    }

    #[test]
    fn lawyer_presets_count() {
        assert_eq!(LAWYER_PRESETS.len(), 10);
    }

    #[test]
    fn all_presets_have_unique_slugs() {
        let mut slugs = std::collections::HashSet::new();
        for p in LAWYER_PRESETS {
            assert!(slugs.insert(p.slug), "duplicate slug: {}", p.slug);
        }
    }

    #[test]
    fn preset_sha256_deterministic() {
        let p = &LAWYER_PRESETS[0];
        let a = preset_sha256(p);
        let b = preset_sha256(p);
        assert_eq!(a, b);
        assert_eq!(a.len(), 64); // SHA256 hex
    }

    #[test]
    fn presets_cover_multiple_categories() {
        let mut categories = std::collections::HashSet::new();
        for p in LAWYER_PRESETS {
            let spec = parse_preset(p).unwrap();
            categories.insert(spec.parent_category);
        }
        // Should touch at least phone, document, daily, interpret
        assert!(categories.contains("phone"));
        assert!(categories.contains("document"));
        assert!(categories.contains("daily"));
        assert!(categories.contains("interpret"));
    }

    #[test]
    fn import_presets_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_workflows_schema(&conn).unwrap();

        let first = import_presets(&conn, LAWYER_PRESETS).unwrap();
        assert_eq!(first, 10);

        let second = import_presets(&conn, LAWYER_PRESETS).unwrap();
        assert_eq!(second, 0, "second import should be a no-op");

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM workflows", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 10);
    }

    #[test]
    fn import_populates_metadata() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_workflows_schema(&conn).unwrap();
        import_presets(&conn, LAWYER_PRESETS).unwrap();

        let (name, category, created_by, sha_len): (String, String, String, usize) = conn
            .query_row(
                "SELECT name, parent_category, created_by, length(spec_sha256) FROM workflows LIMIT 1",
                [],
                |r| {
                    let len = usize::try_from(r.get::<_, i64>(3)?.max(0)).unwrap_or(0);
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, len))
                },
            )
            .unwrap();
        assert!(!name.is_empty());
        assert!(!category.is_empty());
        assert_eq!(created_by, "preset");
        assert_eq!(sha_len, 64);
    }

    #[test]
    fn all_presets_have_limits_with_positive_values() {
        for p in LAWYER_PRESETS {
            let spec = parse_preset(p).unwrap();
            assert!(
                spec.limits.max_tokens_per_run > 0,
                "{}: max_tokens_per_run must be > 0",
                p.slug
            );
            assert!(
                spec.limits.max_llm_calls_per_run > 0,
                "{}: max_llm_calls_per_run must be > 0",
                p.slug
            );
        }
    }

    #[test]
    fn all_presets_have_at_least_one_step() {
        for p in LAWYER_PRESETS {
            let spec = parse_preset(p).unwrap();
            assert!(!spec.steps.is_empty(), "{}: steps must not be empty", p.slug);
        }
    }

    // ── Shopping preset tests ───────────────────────────────────

    #[test]
    fn all_shopping_presets_parse() {
        let failures = validate_all_presets(SHOPPING_PRESETS);
        assert!(
            failures.is_empty(),
            "Some shopping presets failed: {failures:?}"
        );
    }

    #[test]
    fn shopping_presets_use_shopping_category() {
        for p in SHOPPING_PRESETS {
            let spec = parse_preset(p).unwrap();
            assert_eq!(
                spec.parent_category, "shopping",
                "{}: must be parent_category='shopping'",
                p.slug
            );
        }
    }

    #[test]
    fn all_presets_combined() {
        let combined = all_presets();
        assert_eq!(combined.len(), LAWYER_PRESETS.len() + SHOPPING_PRESETS.len());
    }

    #[test]
    fn all_shopping_slugs_prefixed() {
        for p in SHOPPING_PRESETS {
            assert!(
                p.slug.starts_with("shopping_"),
                "slug '{}' must start with 'shopping_'",
                p.slug
            );
        }
    }
}
