use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Participant role in a session
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum ParticipantRole {
    User,
    Agent,
    System,
}

/// A participant in a session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Participant {
    pub id: String,
    pub role: ParticipantRole,
    pub name: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

impl Participant {
    /// Create a new participant
    pub fn new(id: impl Into<String>, role: ParticipantRole) -> Self {
        Self {
            id: id.into(),
            role,
            name: None,
            metadata: None,
        }
    }

    /// Create a user participant
    pub fn user(id: impl Into<String>) -> Self {
        Self::new(id, ParticipantRole::User)
    }

    /// Create an agent participant
    pub fn agent(id: impl Into<String>) -> Self {
        Self::new(id, ParticipantRole::Agent)
    }

    /// Set participant name
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set participant metadata
    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

/// Participant manager for tracking session participants
pub struct ParticipantManager {
    participants: HashMap<String, Participant>,
}

impl ParticipantManager {
    /// Create a new participant manager
    pub fn new() -> Self {
        Self {
            participants: HashMap::new(),
        }
    }

    /// Add a participant
    pub fn add(&mut self, participant: Participant) {
        self.participants
            .insert(participant.id.clone(), participant);
    }

    /// Remove a participant
    pub fn remove(&mut self, id: &str) -> Option<Participant> {
        self.participants.remove(id)
    }

    /// Get a participant
    pub fn get(&self, id: &str) -> Option<&Participant> {
        self.participants.get(id)
    }

    /// List all participants
    pub fn list(&self) -> Vec<&Participant> {
        self.participants.values().collect()
    }

    /// List participants by role
    pub fn list_by_role(&self, role: ParticipantRole) -> Vec<&Participant> {
        self.participants
            .values()
            .filter(|p| p.role == role)
            .collect()
    }

    /// Get participant count
    pub fn count(&self) -> usize {
        self.participants.len()
    }

    /// Get participant count by role
    pub fn count_by_role(&self, role: ParticipantRole) -> usize {
        self.participants
            .values()
            .filter(|p| p.role == role)
            .count()
    }

    /// Check if participant exists
    pub fn contains(&self, id: &str) -> bool {
        self.participants.contains_key(id)
    }

    /// Serialize to JSON
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&self.participants)
    }

    /// Deserialize from JSON
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        let participants: HashMap<String, Participant> = serde_json::from_str(json)?;
        Ok(Self { participants })
    }
}

impl Default for ParticipantManager {
    fn default() -> Self {
        Self::new()
    }
}
