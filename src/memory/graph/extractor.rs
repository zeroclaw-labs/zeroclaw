//! Heuristic entity/concept extractor for graph population.
//!
//! Extracts entities from text using regex patterns and graph lookups.
//! This is the "fast path" — no LLM calls required.

use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

/// An extracted entity from text.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExtractedEntity {
    pub name: String,
    pub entity_type: EntityType,
}

/// Types of entities we can extract heuristically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityType {
    /// Capitalized proper noun
    ProperNoun,
    /// Technical term (programming language, framework, etc.)
    TechTerm,
    /// URL or domain reference
    Url,
    /// Email address
    Email,
    /// Numeric identifier or version
    Version,
    /// Known concept from the existing graph
    KnownConcept,
}

impl std::fmt::Display for EntityType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProperNoun => write!(f, "proper_noun"),
            Self::TechTerm => write!(f, "tech_term"),
            Self::Url => write!(f, "url"),
            Self::Email => write!(f, "email"),
            Self::Version => write!(f, "version"),
            Self::KnownConcept => write!(f, "known_concept"),
        }
    }
}

// Common tech terms for heuristic extraction
static TECH_TERMS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "rust",
        "python",
        "javascript",
        "typescript",
        "java",
        "go",
        "c++",
        "c#",
        "react",
        "vue",
        "angular",
        "svelte",
        "next.js",
        "nuxt",
        "django",
        "flask",
        "fastapi",
        "express",
        "axum",
        "actix",
        "tokio",
        "async",
        "docker",
        "kubernetes",
        "k8s",
        "postgres",
        "postgresql",
        "mysql",
        "sqlite",
        "redis",
        "mongodb",
        "cozodb",
        "datalog",
        "graphql",
        "rest",
        "grpc",
        "websocket",
        "http",
        "linux",
        "macos",
        "windows",
        "android",
        "ios",
        "wasm",
        "webassembly",
        "git",
        "github",
        "gitlab",
        "ci/cd",
        "terraform",
        "ansible",
        "nginx",
        "api",
        "sdk",
        "cli",
        "gui",
        "tui",
        "llm",
        "gpt",
        "claude",
        "gemini",
        "openai",
        "anthropic",
        "embeddings",
        "vector",
        "rag",
        "fine-tuning",
        "machine learning",
        "deep learning",
        "neural network",
        "transformer",
        "cargo",
        "npm",
        "pip",
        "yarn",
        "bun",
        "pnpm",
    ]
    .into_iter()
    .collect()
});

static PROPER_NOUN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b([A-Z][a-záéíóúñ]+(?:\s+[A-Z][a-záéíóúñ]+)*)").unwrap());

static URL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https?://[^\s<>"']+"#).unwrap());

static EMAIL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}").unwrap());

static VERSION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bv?\d+\.\d+(?:\.\d+)?(?:-[a-zA-Z0-9.]+)?\b").unwrap());

/// Extract entities from text using heuristic patterns.
///
/// `known_concepts` is a set of concept names already in the graph,
/// used for exact-match lookups (case-insensitive).
pub fn extract_entities<S: ::std::hash::BuildHasher>(
    text: &str,
    known_concepts: &HashSet<String, S>,
) -> Vec<ExtractedEntity> {
    let mut entities = HashSet::new();
    let text_lower = text.to_lowercase();

    // 1. Match against known graph concepts (case-insensitive)
    for concept in known_concepts {
        let concept_lower = concept.to_lowercase();
        if text_lower.contains(&concept_lower) {
            entities.insert(ExtractedEntity {
                name: concept.clone(),
                entity_type: EntityType::KnownConcept,
            });
        }
    }

    // 2. Tech terms
    for &term in TECH_TERMS.iter() {
        if text_lower.contains(term) {
            entities.insert(ExtractedEntity {
                name: term.to_string(),
                entity_type: EntityType::TechTerm,
            });
        }
    }

    // 3. Proper nouns (capitalized words not at sentence start)
    for cap in PROPER_NOUN_RE.find_iter(text) {
        let name = cap.as_str().to_string();
        // Skip single short words that are likely sentence starts
        if name.len() >= 3 {
            // Skip common sentence starters
            let lower = name.to_lowercase();
            if !matches!(
                lower.as_str(),
                "the"
                    | "this"
                    | "that"
                    | "these"
                    | "those"
                    | "what"
                    | "when"
                    | "where"
                    | "which"
                    | "how"
                    | "who"
                    | "why"
                    | "yes"
                    | "no"
                    | "not"
                    | "but"
                    | "and"
                    | "for"
                    | "are"
                    | "was"
                    | "were"
                    | "will"
                    | "can"
                    | "may"
                    | "let"
                    | "use"
                    | "all"
                    | "has"
                    | "have"
                    | "had"
                    | "get"
                    | "set"
                    | "add"
                    | "new"
                    | "old"
                    | "one"
                    | "two"
                    | "del"
                    | "las"
                    | "los"
                    | "una"
                    | "por"
                    | "con"
                    | "que"
                    | "para"
                    | "como"
                    | "este"
                    | "esta"
                    | "eso"
                    | "esa"
            ) {
                entities.insert(ExtractedEntity {
                    name,
                    entity_type: EntityType::ProperNoun,
                });
            }
        }
    }

    // 4. URLs
    for url_match in URL_RE.find_iter(text) {
        entities.insert(ExtractedEntity {
            name: url_match.as_str().to_string(),
            entity_type: EntityType::Url,
        });
    }

    // 5. Emails
    for email_match in EMAIL_RE.find_iter(text) {
        entities.insert(ExtractedEntity {
            name: email_match.as_str().to_string(),
            entity_type: EntityType::Email,
        });
    }

    // 6. Versions
    for ver_match in VERSION_RE.find_iter(text) {
        let ver = ver_match.as_str().to_string();
        if ver.len() >= 3 {
            entities.insert(ExtractedEntity {
                name: ver,
                entity_type: EntityType::Version,
            });
        }
    }

    entities.into_iter().collect()
}

/// Extract just the entity names for use in graph lookup queries.
pub fn extract_query_terms<S: ::std::hash::BuildHasher>(
    text: &str,
    known_concepts: &HashSet<String, S>,
) -> Vec<String> {
    extract_entities(text, known_concepts)
        .into_iter()
        .map(|e| e.name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_tech_terms() {
        let known = HashSet::new();
        let entities = extract_entities("I'm building a Rust API with tokio and axum", &known);
        let names: HashSet<&str> = entities.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains("rust"));
        assert!(names.contains("tokio"));
        assert!(names.contains("axum"));
    }

    #[test]
    fn extracts_known_concepts() {
        let mut known = HashSet::new();
        known.insert("JhedaiClaw".to_string());
        known.insert("Memory OS".to_string());

        let entities = extract_entities("JhedaiClaw uses Memory OS for knowledge", &known);
        let names: HashSet<&str> = entities.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains("JhedaiClaw"));
        assert!(names.contains("Memory OS"));
    }

    #[test]
    fn extracts_urls() {
        let known = HashSet::new();
        let entities = extract_entities(
            "Check https://github.com/jhedai/jhedaiclaw for details",
            &known,
        );
        let urls: Vec<_> = entities
            .iter()
            .filter(|e| e.entity_type == EntityType::Url)
            .collect();
        assert!(!urls.is_empty());
    }

    #[test]
    fn extract_query_terms_returns_names() {
        let known = HashSet::new();
        let terms = extract_query_terms("Building with Rust and docker", &known);
        assert!(terms.contains(&"rust".to_string()));
        assert!(terms.contains(&"docker".to_string()));
    }
}
