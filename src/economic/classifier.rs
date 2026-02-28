//! Task Classifier for ZeroClaw Economic Agents
//!
//! Classifies work instructions into 44 BLS occupations with wage data
//! to estimate task value for agent economics.
//!
//! ## Overview
//!
//! The classifier matches task instructions to standardized occupation
//! categories using keyword matching and heuristics, then calculates
//! expected payment based on BLS hourly wage data.
//!
//! ## Example
//!
//! ```rust,ignore
//! use zeroclaw::economic::classifier::{TaskClassifier, OccupationCategory};
//!
//! let classifier = TaskClassifier::new();
//! let result = classifier.classify("Write a REST API in Rust").await?;
//!
//! println!("Occupation: {}", result.occupation);
//! println!("Hourly wage: ${:.2}", result.hourly_wage);
//! println!("Estimated hours: {:.2}", result.estimated_hours);
//! println!("Max payment: ${:.2}", result.max_payment);
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Occupation category groupings based on BLS major groups
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OccupationCategory {
    /// Software, IT, engineering roles
    TechnologyEngineering,
    /// Finance, accounting, management, sales
    BusinessFinance,
    /// Medical, nursing, social work
    HealthcareSocialServices,
    /// Legal, media, operations, other professional
    LegalMediaOperations,
}

impl OccupationCategory {
    /// Returns a human-readable name for the category
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::TechnologyEngineering => "Technology & Engineering",
            Self::BusinessFinance => "Business & Finance",
            Self::HealthcareSocialServices => "Healthcare & Social Services",
            Self::LegalMediaOperations => "Legal, Media & Operations",
        }
    }
}

/// A single occupation with BLS wage data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Occupation {
    /// Official BLS occupation name
    pub name: String,
    /// Hourly wage in USD (BLS median)
    pub hourly_wage: f64,
    /// Category grouping
    pub category: OccupationCategory,
    /// Keywords for matching
    #[serde(skip)]
    pub keywords: Vec<&'static str>,
}

/// Result of classifying a task instruction
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassificationResult {
    /// Matched occupation name
    pub occupation: String,
    /// BLS hourly wage for this occupation
    pub hourly_wage: f64,
    /// Estimated hours to complete task
    pub estimated_hours: f64,
    /// Maximum payment (hours × wage)
    pub max_payment: f64,
    /// Classification confidence (0.0 - 1.0)
    pub confidence: f64,
    /// Category of the matched occupation
    pub category: OccupationCategory,
    /// Brief reasoning for the classification
    pub reasoning: String,
}

/// Task classifier that maps instructions to BLS occupations
#[derive(Debug)]
pub struct TaskClassifier {
    occupations: Vec<Occupation>,
    keyword_index: HashMap<&'static str, Vec<usize>>,
    fallback_occupation: String,
    fallback_wage: f64,
}

impl Default for TaskClassifier {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskClassifier {
    /// Create a new TaskClassifier with embedded BLS occupation data
    pub fn new() -> Self {
        let occupations = Self::load_occupations();
        let keyword_index = Self::build_keyword_index(&occupations);

        Self {
            occupations,
            keyword_index,
            fallback_occupation: "General and Operations Managers".to_string(),
            fallback_wage: 64.0,
        }
    }

    /// Load all 44 BLS occupations with wage data
    fn load_occupations() -> Vec<Occupation> {
        use OccupationCategory::*;

        vec![
            // Technology & Engineering
            Occupation {
                name: "Software Developers".into(),
                hourly_wage: 69.50,
                category: TechnologyEngineering,
                keywords: vec![
                    "software",
                    "code",
                    "programming",
                    "developer",
                    "rust",
                    "python",
                    "javascript",
                    "api",
                    "backend",
                    "frontend",
                    "fullstack",
                    "app",
                    "application",
                    "debug",
                    "refactor",
                    "implement",
                    "algorithm",
                ],
            },
            Occupation {
                name: "Computer and Information Systems Managers".into(),
                hourly_wage: 90.38,
                category: TechnologyEngineering,
                keywords: vec![
                    "it manager",
                    "cto",
                    "tech lead",
                    "infrastructure",
                    "systems",
                    "devops",
                    "cloud",
                    "architecture",
                    "platform",
                    "enterprise",
                ],
            },
            Occupation {
                name: "Industrial Engineers".into(),
                hourly_wage: 51.87,
                category: TechnologyEngineering,
                keywords: vec![
                    "industrial",
                    "process",
                    "optimization",
                    "efficiency",
                    "workflow",
                    "manufacturing",
                    "lean",
                    "six sigma",
                    "production",
                ],
            },
            Occupation {
                name: "Mechanical Engineers".into(),
                hourly_wage: 52.92,
                category: TechnologyEngineering,
                keywords: vec![
                    "mechanical",
                    "cad",
                    "solidworks",
                    "machinery",
                    "thermal",
                    "hvac",
                    "automotive",
                    "robotics",
                ],
            },
            // Business & Finance
            Occupation {
                name: "Accountants and Auditors".into(),
                hourly_wage: 44.96,
                category: BusinessFinance,
                keywords: vec![
                    "accounting",
                    "audit",
                    "tax",
                    "bookkeeping",
                    "financial statements",
                    "gaap",
                    "ledger",
                    "reconciliation",
                    "cpa",
                ],
            },
            Occupation {
                name: "Administrative Services Managers".into(),
                hourly_wage: 60.59,
                category: BusinessFinance,
                keywords: vec![
                    "administrative",
                    "office manager",
                    "facilities",
                    "operations",
                    "scheduling",
                    "coordination",
                ],
            },
            Occupation {
                name: "Buyers and Purchasing Agents".into(),
                hourly_wage: 39.29,
                category: BusinessFinance,
                keywords: vec![
                    "procurement",
                    "purchasing",
                    "vendor",
                    "supplier",
                    "sourcing",
                    "negotiation",
                    "contracts",
                ],
            },
            Occupation {
                name: "Compliance Officers".into(),
                hourly_wage: 40.86,
                category: BusinessFinance,
                keywords: vec![
                    "compliance",
                    "regulatory",
                    "audit",
                    "policy",
                    "governance",
                    "risk",
                    "sox",
                    "gdpr",
                ],
            },
            Occupation {
                name: "Financial Managers".into(),
                hourly_wage: 86.76,
                category: BusinessFinance,
                keywords: vec![
                    "cfo",
                    "finance director",
                    "treasury",
                    "budget",
                    "financial planning",
                    "investment management",
                ],
            },
            Occupation {
                name: "Financial and Investment Analysts".into(),
                hourly_wage: 56.01,
                category: BusinessFinance,
                keywords: vec![
                    "financial analysis",
                    "investment",
                    "portfolio",
                    "stock",
                    "equity",
                    "valuation",
                    "modeling",
                    "dcf",
                    "market research",
                ],
            },
            Occupation {
                name: "General and Operations Managers".into(),
                hourly_wage: 64.00,
                category: BusinessFinance,
                keywords: vec![
                    "operations",
                    "general manager",
                    "director",
                    "oversee",
                    "manage",
                    "strategy",
                    "leadership",
                    "business",
                ],
            },
            Occupation {
                name: "Market Research Analysts and Marketing Specialists".into(),
                hourly_wage: 41.58,
                category: BusinessFinance,
                keywords: vec![
                    "market research",
                    "marketing",
                    "campaign",
                    "branding",
                    "seo",
                    "advertising",
                    "analytics",
                    "customer",
                    "segment",
                ],
            },
            Occupation {
                name: "Personal Financial Advisors".into(),
                hourly_wage: 77.02,
                category: BusinessFinance,
                keywords: vec![
                    "financial advisor",
                    "wealth",
                    "retirement",
                    "401k",
                    "ira",
                    "estate planning",
                    "insurance",
                ],
            },
            Occupation {
                name: "Project Management Specialists".into(),
                hourly_wage: 51.97,
                category: BusinessFinance,
                keywords: vec![
                    "project manager",
                    "pmp",
                    "agile",
                    "scrum",
                    "sprint",
                    "milestone",
                    "timeline",
                    "stakeholder",
                    "deliverable",
                ],
            },
            Occupation {
                name: "Property, Real Estate, and Community Association Managers".into(),
                hourly_wage: 39.77,
                category: BusinessFinance,
                keywords: vec![
                    "property",
                    "real estate",
                    "landlord",
                    "tenant",
                    "lease",
                    "hoa",
                    "community",
                ],
            },
            Occupation {
                name: "Sales Managers".into(),
                hourly_wage: 77.37,
                category: BusinessFinance,
                keywords: vec![
                    "sales manager",
                    "revenue",
                    "quota",
                    "pipeline",
                    "crm",
                    "account executive",
                    "territory",
                ],
            },
            Occupation {
                name: "Marketing and Sales Managers".into(),
                hourly_wage: 79.35,
                category: BusinessFinance,
                keywords: vec!["vp sales", "cmo", "growth", "go-to-market", "demand gen"],
            },
            Occupation {
                name: "Financial Specialists".into(),
                hourly_wage: 48.12,
                category: BusinessFinance,
                keywords: vec!["financial specialist", "credit", "loan", "underwriting"],
            },
            Occupation {
                name: "Securities, Commodities, and Financial Services Sales Agents".into(),
                hourly_wage: 48.12,
                category: BusinessFinance,
                keywords: vec!["broker", "securities", "commodities", "trading", "series 7"],
            },
            Occupation {
                name: "Business Operations Specialists, All Other".into(),
                hourly_wage: 44.41,
                category: BusinessFinance,
                keywords: vec![
                    "business analyst",
                    "operations specialist",
                    "process improvement",
                ],
            },
            Occupation {
                name: "Claims Adjusters, Examiners, and Investigators".into(),
                hourly_wage: 37.87,
                category: BusinessFinance,
                keywords: vec!["claims", "insurance", "adjuster", "investigator", "fraud"],
            },
            Occupation {
                name: "Transportation, Storage, and Distribution Managers".into(),
                hourly_wage: 55.77,
                category: BusinessFinance,
                keywords: vec![
                    "logistics",
                    "supply chain",
                    "warehouse",
                    "distribution",
                    "shipping",
                    "inventory",
                    "fulfillment",
                ],
            },
            Occupation {
                name: "Industrial Production Managers".into(),
                hourly_wage: 62.11,
                category: BusinessFinance,
                keywords: vec![
                    "production manager",
                    "plant manager",
                    "manufacturing operations",
                ],
            },
            Occupation {
                name: "Lodging Managers".into(),
                hourly_wage: 37.24,
                category: BusinessFinance,
                keywords: vec!["hotel", "hospitality", "lodging", "resort", "concierge"],
            },
            Occupation {
                name: "Real Estate Brokers".into(),
                hourly_wage: 39.77,
                category: BusinessFinance,
                keywords: vec!["real estate broker", "realtor", "mls", "listing"],
            },
            Occupation {
                name: "Managers, All Other".into(),
                hourly_wage: 72.06,
                category: BusinessFinance,
                keywords: vec!["manager", "supervisor", "team lead"],
            },
            // Healthcare & Social Services
            Occupation {
                name: "Medical and Health Services Managers".into(),
                hourly_wage: 66.22,
                category: HealthcareSocialServices,
                keywords: vec![
                    "healthcare",
                    "hospital",
                    "clinic",
                    "medical",
                    "health services",
                    "patient",
                    "hipaa",
                ],
            },
            Occupation {
                name: "Social and Community Service Managers".into(),
                hourly_wage: 41.39,
                category: HealthcareSocialServices,
                keywords: vec![
                    "social services",
                    "community",
                    "nonprofit",
                    "outreach",
                    "case management",
                    "welfare",
                ],
            },
            Occupation {
                name: "Child, Family, and School Social Workers".into(),
                hourly_wage: 41.39,
                category: HealthcareSocialServices,
                keywords: vec![
                    "social worker",
                    "child welfare",
                    "family services",
                    "school counselor",
                ],
            },
            Occupation {
                name: "Registered Nurses".into(),
                hourly_wage: 66.22,
                category: HealthcareSocialServices,
                keywords: vec!["nurse", "rn", "nursing", "patient care", "clinical"],
            },
            Occupation {
                name: "Nurse Practitioners".into(),
                hourly_wage: 66.22,
                category: HealthcareSocialServices,
                keywords: vec!["np", "nurse practitioner", "aprn", "prescribe"],
            },
            Occupation {
                name: "Pharmacists".into(),
                hourly_wage: 66.22,
                category: HealthcareSocialServices,
                keywords: vec![
                    "pharmacy",
                    "pharmacist",
                    "medication",
                    "prescription",
                    "drug",
                ],
            },
            Occupation {
                name: "Medical Secretaries and Administrative Assistants".into(),
                hourly_wage: 66.22,
                category: HealthcareSocialServices,
                keywords: vec![
                    "medical secretary",
                    "medical records",
                    "ehr",
                    "scheduling appointments",
                ],
            },
            // Legal, Media & Operations
            Occupation {
                name: "Lawyers".into(),
                hourly_wage: 44.41,
                category: LegalMediaOperations,
                keywords: vec![
                    "lawyer",
                    "attorney",
                    "legal",
                    "contract",
                    "litigation",
                    "counsel",
                    "law",
                    "paralegal",
                ],
            },
            Occupation {
                name: "Editors".into(),
                hourly_wage: 72.06,
                category: LegalMediaOperations,
                keywords: vec![
                    "editor",
                    "editing",
                    "proofread",
                    "copy edit",
                    "manuscript",
                    "publication",
                ],
            },
            Occupation {
                name: "Film and Video Editors".into(),
                hourly_wage: 68.15,
                category: LegalMediaOperations,
                keywords: vec![
                    "video editor",
                    "film",
                    "premiere",
                    "final cut",
                    "davinci",
                    "post-production",
                ],
            },
            Occupation {
                name: "Audio and Video Technicians".into(),
                hourly_wage: 41.86,
                category: LegalMediaOperations,
                keywords: vec![
                    "audio",
                    "video",
                    "av",
                    "broadcast",
                    "streaming",
                    "recording",
                ],
            },
            Occupation {
                name: "Producers and Directors".into(),
                hourly_wage: 41.86,
                category: LegalMediaOperations,
                keywords: vec![
                    "producer",
                    "director",
                    "production",
                    "creative director",
                    "content",
                    "show",
                ],
            },
            Occupation {
                name: "News Analysts, Reporters, and Journalists".into(),
                hourly_wage: 68.15,
                category: LegalMediaOperations,
                keywords: vec![
                    "journalist",
                    "reporter",
                    "news",
                    "article",
                    "press",
                    "interview",
                    "story",
                ],
            },
            Occupation {
                name: "Entertainment and Recreation Managers, Except Gambling".into(),
                hourly_wage: 41.86,
                category: LegalMediaOperations,
                keywords: vec!["entertainment", "recreation", "event", "venue", "concert"],
            },
            Occupation {
                name: "Recreation Workers".into(),
                hourly_wage: 41.86,
                category: LegalMediaOperations,
                keywords: vec!["recreation", "activity", "fitness", "sports"],
            },
            Occupation {
                name: "Customer Service Representatives".into(),
                hourly_wage: 44.41,
                category: LegalMediaOperations,
                keywords: vec!["customer service", "support", "helpdesk", "ticket", "chat"],
            },
            Occupation {
                name: "Private Detectives and Investigators".into(),
                hourly_wage: 37.87,
                category: LegalMediaOperations,
                keywords: vec![
                    "detective",
                    "investigator",
                    "background check",
                    "surveillance",
                ],
            },
            Occupation {
                name: "First-Line Supervisors of Police and Detectives".into(),
                hourly_wage: 72.06,
                category: LegalMediaOperations,
                keywords: vec!["police", "law enforcement", "security supervisor"],
            },
        ]
    }

    /// Build keyword → occupation index for fast lookup
    fn build_keyword_index(occupations: &[Occupation]) -> HashMap<&'static str, Vec<usize>> {
        let mut index: HashMap<&'static str, Vec<usize>> = HashMap::new();
        for (i, occ) in occupations.iter().enumerate() {
            for &kw in &occ.keywords {
                index.entry(kw).or_default().push(i);
            }
        }
        index
    }

    /// Classify a task instruction into an occupation with estimated value
    ///
    /// This is a synchronous keyword-based classifier. For LLM-based
    /// classification, use `classify_with_llm` instead.
    pub fn classify(&self, instruction: &str) -> ClassificationResult {
        let lower = instruction.to_lowercase();
        let mut scores: HashMap<usize, f64> = HashMap::new();

        // Score each occupation by keyword matches
        for (keyword, occ_indices) in &self.keyword_index {
            if lower.contains(keyword) {
                for &idx in occ_indices {
                    *scores.entry(idx).or_default() += 1.0;
                }
            }
        }

        // Find best match
        let (best_idx, best_score) = scores
            .iter()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(&idx, &score)| (idx, score))
            .unwrap_or((usize::MAX, 0.0));

        let (occupation, hourly_wage, category, confidence, reasoning) =
            if best_idx < self.occupations.len() {
                let occ = &self.occupations[best_idx];
                let confidence = (best_score / 3.0).min(1.0); // Normalize confidence
                (
                    occ.name.clone(),
                    occ.hourly_wage,
                    occ.category,
                    confidence,
                    format!("Matched {} keywords", best_score as i32),
                )
            } else {
                // Fallback
                (
                    self.fallback_occupation.clone(),
                    self.fallback_wage,
                    OccupationCategory::BusinessFinance,
                    0.3,
                    "Fallback classification - no strong keyword match".to_string(),
                )
            };

        let estimated_hours = Self::estimate_hours(instruction);
        let max_payment = (estimated_hours * hourly_wage * 100.0).round() / 100.0;

        ClassificationResult {
            occupation,
            hourly_wage,
            estimated_hours,
            max_payment,
            confidence,
            category,
            reasoning,
        }
    }

    /// Estimate hours based on instruction complexity
    fn estimate_hours(instruction: &str) -> f64 {
        let word_count = instruction.split_whitespace().count();
        let has_complex_markers = instruction.to_lowercase().contains("implement")
            || instruction.contains("build")
            || instruction.contains("create")
            || instruction.contains("design")
            || instruction.contains("develop");

        let has_simple_markers = instruction.to_lowercase().contains("fix")
            || instruction.contains("update")
            || instruction.contains("change")
            || instruction.contains("review");

        let base_hours = if has_complex_markers {
            2.0
        } else if has_simple_markers {
            0.5
        } else {
            1.0
        };

        // Scale by instruction length
        let length_factor = (word_count as f64 / 20.0).max(0.5).min(2.0);
        let hours = base_hours * length_factor;

        // Clamp to valid range
        hours.max(0.25).min(40.0)
    }

    /// Get all occupations
    pub fn occupations(&self) -> &[Occupation] {
        &self.occupations
    }

    /// Get occupations by category
    pub fn occupations_by_category(&self, category: OccupationCategory) -> Vec<&Occupation> {
        self.occupations
            .iter()
            .filter(|o| o.category == category)
            .collect()
    }

    /// Get the fallback occupation name
    pub fn fallback_occupation(&self) -> &str {
        &self.fallback_occupation
    }

    /// Get the fallback hourly wage
    pub fn fallback_wage(&self) -> f64 {
        self.fallback_wage
    }

    /// Look up an occupation by exact name
    pub fn get_occupation(&self, name: &str) -> Option<&Occupation> {
        self.occupations.iter().find(|o| o.name == name)
    }

    /// Fuzzy match an occupation name (case-insensitive, substring)
    pub fn fuzzy_match(&self, name: &str) -> Option<&Occupation> {
        let lower = name.to_lowercase();

        // Exact match first
        if let Some(occ) = self.occupations.iter().find(|o| o.name == name) {
            return Some(occ);
        }

        // Case-insensitive match
        if let Some(occ) = self
            .occupations
            .iter()
            .find(|o| o.name.to_lowercase() == lower)
        {
            return Some(occ);
        }

        // Substring match
        self.occupations.iter().find(|o| {
            lower.contains(&o.name.to_lowercase()) || o.name.to_lowercase().contains(&lower)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classifier_new() {
        let classifier = TaskClassifier::new();
        assert_eq!(classifier.occupations.len(), 44);
    }

    #[test]
    fn test_classify_software() {
        let classifier = TaskClassifier::new();
        let result = classifier.classify("Write a REST API in Rust with authentication");

        assert_eq!(result.occupation, "Software Developers");
        assert!((result.hourly_wage - 69.50).abs() < 0.01);
        assert!(result.confidence > 0.0);
    }

    #[test]
    fn test_classify_finance() {
        let classifier = TaskClassifier::new();
        let result = classifier.classify("Prepare quarterly financial statements and audit trail");

        assert!(
            result.occupation.contains("Account") || result.occupation.contains("Financial"),
            "Expected finance occupation, got: {}",
            result.occupation
        );
    }

    #[test]
    fn test_classify_fallback() {
        let classifier = TaskClassifier::new();
        let result = classifier.classify("xyzzy foobar baz");

        assert_eq!(result.occupation, "General and Operations Managers");
        assert_eq!(result.confidence, 0.3);
    }

    #[test]
    fn test_estimate_hours_complex() {
        let hours = TaskClassifier::estimate_hours(
            "Implement a complete microservices architecture with event sourcing",
        );
        assert!(hours >= 1.0, "Complex task should estimate >= 1 hour");
    }

    #[test]
    fn test_estimate_hours_simple() {
        let hours = TaskClassifier::estimate_hours("Fix typo");
        assert!(hours <= 1.0, "Simple task should estimate <= 1 hour");
    }

    #[test]
    fn test_fuzzy_match() {
        let classifier = TaskClassifier::new();

        // Exact match
        assert!(classifier.fuzzy_match("Software Developers").is_some());

        // Case insensitive
        assert!(classifier.fuzzy_match("software developers").is_some());

        // Substring
        assert!(classifier.fuzzy_match("Software").is_some());
    }

    #[test]
    fn test_occupations_by_category() {
        let classifier = TaskClassifier::new();
        let tech = classifier.occupations_by_category(OccupationCategory::TechnologyEngineering);

        assert!(!tech.is_empty());
        assert!(tech.iter().any(|o| o.name == "Software Developers"));
    }
}
