//! Canonical filenames + scaffold subdirs for the Agent Skills spec.
//!
//! Every literal that names a skill-file or scaffold-subdir lives here. Any
//! grep hit for `"SKILL.md"`, `"scripts"`, `"references"`, `"assets"` outside
//! this module is drift.

/// Canonical manifest filename per the open Agent Skills spec.
pub const SKILL_MANIFEST_FILENAME: &str = "SKILL.md";

/// Pre-spec manifest filenames still accepted by the audit loader for
/// back-compat with installed skills. Never written by the service.
pub const SKILL_DEPRECATED_MANIFESTS: &[&str] = &["SKILL.toml", "manifest.toml"];

/// Optional standard subdirs scaffolded under each new skill directory.
/// Match the canonical agentskills.io layout (`scripts/`, `references/`,
/// `assets/`).
pub const SKILL_SCAFFOLD_SUBDIRS: &[&str] = &["scripts", "references", "assets"];

/// Archive root under the shared workspace where deleted skills are moved
/// (when `RemoveMode::Archive` is selected). Mirrors the agent-workspace
/// archive convention.
pub const SKILL_ARCHIVE_DIR_NAME: &str = "_deleted";
