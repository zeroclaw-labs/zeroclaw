use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// =============================================================================
// Status Enums
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DomainType {
    Personal,
    Professional,
    Project,
    Health,
    Finance,
    Learning,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EpicStatus {
    NotStarted,
    InProgress,
    Completed,
    OnHold,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SprintStatus {
    NotStarted,
    Active,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    OnHold,
    Approved,
    NextUp,
    Future,
    InProgress,
    InReview,
    Archived,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    P1Urgent,
    P2High,
    P3Medium,
    P4Low,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    Feature,
    Fix,
    Hotfix,
    Chore,
    Docs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskCategory {
    SoftwareDev,
    General,
    FinancialAdmin,
    FilmProduction,
    ContentCreation,
    Photography,
    BusinessAdmin,
    Personal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UserStoryStatus {
    Draft,
    Interviewing,
    Discovered,
    Ready,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UserStoryPriority {
    MustHave,
    ShouldHave,
    CouldHave,
    WontHave,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryStatus {
    NotStarted,
    InProgress,
    Completed,
    Blocked,
}

// =============================================================================
// Models (mirror Django 1:1)
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifeDomain {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: String,
    #[serde(rename = "type")]
    pub domain_type: String,
    pub icon: String,
    pub color: String,
    pub is_active: bool,
    pub notion_id: String,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Epic {
    pub id: Uuid,
    pub name: String,
    pub domain_id: Option<String>,
    pub status: String,
    pub subtitle: String,
    pub log_line: String,
    pub priority: String,
    pub start_date: Option<String>,
    pub target_date: Option<String>,
    pub github_repo: String,
    pub notion_id: String,
    pub budget_tier: String,
    pub estimated_budget: Option<f64>,
    pub client: String,
    pub production_company: String,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sprint {
    pub id: Uuid,
    pub name: String,
    pub domain_id: Option<String>,
    pub epic_id: Option<String>,
    pub status: String,
    pub objectives: String,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub quality_score: Option<f64>,
    pub notion_id: String,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub acceptance_criteria: String,
    pub domain_id: Option<String>,
    pub epic_id: Option<String>,
    pub sprint_id: Option<String>,
    pub user_story_id: Option<String>,
    pub parent_task_id: Option<String>,
    pub status: String,
    pub priority: String,
    pub task_type: String,
    pub task_category: String,
    pub due_date: Option<String>,
    pub do_date: Option<String>,
    pub agent_status: String,
    pub assigned_agent: String,
    pub branch_name: String,
    pub pr_url: String,
    pub ai_summary: String,
    pub note: String,
    pub notion_id: String,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserStory {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    /// JSON stored as text in SQLite
    pub acceptance_criteria: String,
    pub epic_id: Option<String>,
    pub sprint_id: Option<String>,
    pub status: String,
    pub user_type: String,
    pub priority: String,
    pub story_points: Option<i64>,
    pub notion_id: String,
    pub current_interview_round: String,
    pub discovery_status: String,
    /// JSON stored as text in SQLite
    pub interview_transcript: String,
    /// JSON stored as text in SQLite
    pub personas: String,
    /// JSON stored as text in SQLite
    pub user_flows: String,
    /// JSON stored as text in SQLite
    pub edge_cases: String,
    /// JSON stored as text in SQLite
    pub technical_constraints: String,
    /// JSON stored as text in SQLite
    pub rbac_requirements: String,
    pub research_notes: String,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}
