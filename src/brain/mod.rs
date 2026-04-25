//! Brain vector DB — indexes and queries ~/.brain/ files.
//!
//! Provides local ONNX embeddings (BGE-small-en-v1.5 via fastembed),
//! YAML-aware chunking, SQLite FTS5 + cosine similarity hybrid search,
//! and content-hash validation.

pub mod cli;
pub mod compile;
pub mod db;
pub mod index;
#[cfg(feature = "brain")]
pub mod onnx_embedding;
pub mod query;
pub mod yaml_chunker;

use std::path::{Path, PathBuf};

/// Top-level Brain DB handle.
pub struct BrainIndex {
    pub db_path: PathBuf,
    pub brain_dir: PathBuf,
}

impl BrainIndex {
    pub fn new(brain_dir: &Path) -> Self {
        Self {
            db_path: brain_dir.join("brain.db"),
            brain_dir: brain_dir.to_path_buf(),
        }
    }
}

/// Categories for brain chunks, derived from file path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkCategory {
    Soul,
    Cortex,
    Knowledge,
    Memory,
    Skill,
    Logic,
    Principle,
    Governance,
    Diagram,
}

impl ChunkCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Soul => "soul",
            Self::Cortex => "cortex",
            Self::Knowledge => "knowledge",
            Self::Memory => "memory",
            Self::Skill => "skill",
            Self::Logic => "logic",
            Self::Principle => "principle",
            Self::Governance => "governance",
            Self::Diagram => "diagram",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "soul" => Some(Self::Soul),
            "cortex" => Some(Self::Cortex),
            "knowledge" => Some(Self::Knowledge),
            "memory" => Some(Self::Memory),
            "skill" => Some(Self::Skill),
            "logic" => Some(Self::Logic),
            "principle" => Some(Self::Principle),
            "governance" => Some(Self::Governance),
            "diagram" => Some(Self::Diagram),
            _ => None,
        }
    }
}

/// File types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileType {
    Yaml,
    Markdown,
    Mermaid,
}

impl FileType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Yaml => "yaml",
            Self::Markdown => "markdown",
            Self::Mermaid => "mermaid",
        }
    }
}

/// Classify a brain file path into category, file type, and optional subject.
pub fn classify_file(rel_path: &str) -> (ChunkCategory, FileType, Option<String>) {
    let lower = rel_path.to_lowercase();

    let file_type = if lower.ends_with(".yaml") || lower.ends_with(".yml") {
        FileType::Yaml
    } else if lower.ends_with(".mmd") || lower.ends_with(".mermaid") {
        FileType::Mermaid
    } else {
        FileType::Markdown
    };

    let parts: Vec<&str> = rel_path.split('/').collect();

    let (category, subject) = match parts.first().copied() {
        Some("soul") => (ChunkCategory::Soul, None),
        Some("cortex") => {
            let subject = if parts.len() >= 3 {
                let filename = parts.last().unwrap_or(&"");
                let stem = filename
                    .rsplit_once('.')
                    .map(|(s, _)| s)
                    .unwrap_or(filename);
                if stem.starts_with('_') {
                    None
                } else {
                    Some(stem.to_string())
                }
            } else {
                None
            };
            if lower.contains("diagrams/") {
                (ChunkCategory::Diagram, subject)
            } else {
                (ChunkCategory::Cortex, subject)
            }
        }
        Some("memory") => (ChunkCategory::Memory, None),
        Some("skills") => (ChunkCategory::Skill, None),
        Some("logic") => (ChunkCategory::Logic, None),
        Some("principles") => (ChunkCategory::Principle, None),
        Some("governance") => (ChunkCategory::Governance, None),
        _ => (ChunkCategory::Knowledge, None),
    };

    (category, file_type, subject)
}

/// Resolve the brain directory from $BRAIN env or default to ~/.brain/.
pub fn brain_dir() -> PathBuf {
    if let Ok(val) = std::env::var("BRAIN") {
        return PathBuf::from(val);
    }
    directories::UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".brain")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_soul_file() {
        let (cat, ft, subj) = classify_file("soul/identity.yaml");
        assert_eq!(cat, ChunkCategory::Soul);
        assert_eq!(ft, FileType::Yaml);
        assert!(subj.is_none());
    }

    #[test]
    fn classify_cortex_engineering() {
        let (cat, ft, subj) = classify_file("cortex/engineering/backend.yaml");
        assert_eq!(cat, ChunkCategory::Cortex);
        assert_eq!(ft, FileType::Yaml);
        assert_eq!(subj.as_deref(), Some("backend"));
    }

    #[test]
    fn classify_cortex_shared() {
        let (_, _, subj) = classify_file("cortex/business/_shared.yaml");
        assert!(subj.is_none());
    }

    #[test]
    fn classify_diagram() {
        let (cat, ft, _) = classify_file("cortex/diagrams/auth-flow.mmd");
        assert_eq!(cat, ChunkCategory::Diagram);
        assert_eq!(ft, FileType::Mermaid);
    }

    #[test]
    fn classify_knowledge_markdown() {
        let (cat, ft, _) = classify_file("knowledge/architecture.md");
        assert_eq!(cat, ChunkCategory::Knowledge);
        assert_eq!(ft, FileType::Markdown);
    }

    #[test]
    fn classify_skill() {
        let (cat, _, _) = classify_file("skills/automated_audit.yaml");
        assert_eq!(cat, ChunkCategory::Skill);
    }
}
