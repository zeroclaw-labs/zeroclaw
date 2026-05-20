//! Scaffold a new skill on disk: write `SKILL.md` + create optional
//! `scripts/`, `references/`, `assets/` subdirs per the canonical layout.

use std::path::{Path, PathBuf};

use super::bundle::{self, BundleError};
use super::constants::{SKILL_MANIFEST_FILENAME, SKILL_SCAFFOLD_SUBDIRS};
use super::document::SkillDocument;
use super::frontmatter::SkillFrontmatter;
use super::reference::SkillRef;
use zeroclaw_config::schema::Config;

#[derive(Debug, Clone)]
pub struct ScaffoldOptions {
    /// When `true`, also `mkdir -p` the canonical optional subdirs
    /// (`scripts/`, `references/`, `assets/`).
    pub create_optional_subdirs: bool,
    /// Initial markdown body. When empty, defaults to a single H1 heading
    /// matching the skill name.
    pub body: String,
}

impl Default for ScaffoldOptions {
    fn default() -> Self {
        Self {
            create_optional_subdirs: true,
            body: String::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScaffoldError {
    #[error(transparent)]
    Bundle(#[from] BundleError),

    #[error("skill name '{0}' must use lowercase letters, digits, and hyphens only")]
    InvalidName(String),

    #[error("skill '{0}' already exists at {1}")]
    AlreadyExists(String, PathBuf),

    #[error("failed to write skill scaffold: {0}")]
    Io(#[from] std::io::Error),
}

/// Validate the lowercase-hyphen rule per the agent-skills spec name field.
pub fn validate_name(name: &str) -> Result<(), ScaffoldError> {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-');
    if ok {
        Ok(())
    } else {
        Err(ScaffoldError::InvalidName(name.to_string()))
    }
}

/// Materialize a new skill on disk. Idempotency is **not** assumed: if the
/// skill dir already exists, an error is returned.
pub fn scaffold_skill(
    config: &Config,
    install_root: &Path,
    target: &SkillRef,
    frontmatter: SkillFrontmatter,
    opts: ScaffoldOptions,
) -> Result<PathBuf, ScaffoldError> {
    validate_name(target.name())?;

    let bundle_dir = bundle::resolve_directory(config, install_root, target.bundle())?;
    bundle::validate_directory(&bundle_dir, install_root)?;

    let skill_dir = bundle_dir.join(target.name());
    if skill_dir.exists() {
        return Err(ScaffoldError::AlreadyExists(target.to_string(), skill_dir));
    }

    std::fs::create_dir_all(&skill_dir)?;

    let body = if opts.body.is_empty() {
        format!("# {}\n", title_from_name(target.name()))
    } else {
        opts.body
    };
    let document = SkillDocument { frontmatter, body };
    std::fs::write(
        skill_dir.join(SKILL_MANIFEST_FILENAME),
        document.serialize(),
    )?;

    if opts.create_optional_subdirs {
        for sub in SKILL_SCAFFOLD_SUBDIRS {
            std::fs::create_dir_all(skill_dir.join(sub))?;
        }
    }

    Ok(skill_dir)
}

fn title_from_name(name: &str) -> String {
    name.split('-')
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use zeroclaw_config::schema::SkillBundleConfig;

    fn fixture() -> (TempDir, Config) {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.skill_bundles
            .insert("alpha".to_string(), SkillBundleConfig::default());
        (dir, cfg)
    }

    fn skill_ref(bundle: &str, name: &str) -> SkillRef {
        // Tests bypass the resolver; service consumers always go through it.
        SkillRef::new_unchecked(bundle.to_string(), name.to_string())
    }

    #[test]
    fn rejects_invalid_skill_names() {
        for bad in [
            "",
            "-leading",
            "trailing-",
            "Upper",
            "with_underscore",
            "has space",
        ] {
            assert!(
                validate_name(bad).is_err(),
                "expected '{bad}' to be rejected"
            );
        }
    }

    #[test]
    fn accepts_valid_skill_names() {
        for good in ["code-review", "x", "single-word-name", "version2", "abc123"] {
            validate_name(good).unwrap();
        }
    }

    #[test]
    fn scaffold_creates_full_canonical_layout() {
        let (dir, cfg) = fixture();
        let frontmatter = SkillFrontmatter {
            name: "code-review".into(),
            description: "Reviews PRs.".into(),
            ..Default::default()
        };
        let path = scaffold_skill(
            &cfg,
            dir.path(),
            &skill_ref("alpha", "code-review"),
            frontmatter.clone(),
            ScaffoldOptions::default(),
        )
        .unwrap();

        assert!(path.join(SKILL_MANIFEST_FILENAME).exists());
        for sub in SKILL_SCAFFOLD_SUBDIRS {
            assert!(path.join(sub).is_dir(), "missing optional subdir {sub}");
        }

        let written = std::fs::read_to_string(path.join(SKILL_MANIFEST_FILENAME)).unwrap();
        let doc = SkillDocument::parse(&written).unwrap();
        assert_eq!(doc.frontmatter, frontmatter);
        assert!(doc.body.contains("# Code Review"));
    }

    #[test]
    fn scaffold_skips_optional_subdirs_when_disabled() {
        let (dir, cfg) = fixture();
        let path = scaffold_skill(
            &cfg,
            dir.path(),
            &skill_ref("alpha", "minimal"),
            SkillFrontmatter {
                name: "minimal".into(),
                description: "d".into(),
                ..Default::default()
            },
            ScaffoldOptions {
                create_optional_subdirs: false,
                body: String::new(),
            },
        )
        .unwrap();
        assert!(path.join(SKILL_MANIFEST_FILENAME).exists());
        for sub in SKILL_SCAFFOLD_SUBDIRS {
            assert!(!path.join(sub).exists());
        }
    }

    #[test]
    fn scaffold_errors_when_skill_already_exists() {
        let (dir, cfg) = fixture();
        let r = skill_ref("alpha", "dup");
        let fm = SkillFrontmatter {
            name: "dup".into(),
            description: "d".into(),
            ..Default::default()
        };
        scaffold_skill(&cfg, dir.path(), &r, fm.clone(), ScaffoldOptions::default()).unwrap();
        let err = scaffold_skill(&cfg, dir.path(), &r, fm, ScaffoldOptions::default()).unwrap_err();
        assert!(matches!(err, ScaffoldError::AlreadyExists(_, _)));
    }

    #[test]
    fn scaffold_errors_when_bundle_unknown() {
        let (dir, cfg) = fixture();
        let r = skill_ref("missing-bundle", "x");
        let fm = SkillFrontmatter {
            name: "x".into(),
            description: "d".into(),
            ..Default::default()
        };
        let err = scaffold_skill(&cfg, dir.path(), &r, fm, ScaffoldOptions::default()).unwrap_err();
        assert!(matches!(
            err,
            ScaffoldError::Bundle(BundleError::UnknownBundle(_))
        ));
    }

    #[test]
    fn title_from_name_capitalizes_hyphen_segments() {
        assert_eq!(title_from_name("code-review"), "Code Review");
        assert_eq!(title_from_name("x"), "X");
        assert_eq!(
            title_from_name("multi-word-skill-name"),
            "Multi Word Skill Name"
        );
    }
}
