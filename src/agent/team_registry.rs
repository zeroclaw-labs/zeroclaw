//! Team registry for managing team definitions.
//!
//! This module provides a registry for discovering, loading, and managing
//! team definitions from YAML files in the teams directory.

use super::team_definition::{
    AgentRole, BudgetTier, TeamDefinition, TeamTopologyType,
};
use anyhow::{bail, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};
use std::sync::Arc;
use std::fs;
use tokio::sync::RwLock;

/// Team registry for managing team definitions.
pub struct TeamRegistry {
    /// Teams directory
    teams_dir: PathBuf,

    /// Security policy reference
    #[allow(dead_code)]
    security: Arc<crate::security::SecurityPolicy>,

    /// Loaded team definitions
    teams: RwLock<HashMap<String, TeamDefinition>>,
}

impl TeamRegistry {
    /// Create a new team registry.
    pub fn new(
        teams_dir: PathBuf,
        security: Arc<crate::security::SecurityPolicy>,
    ) -> Result<Self> {
        fs::create_dir_all(&teams_dir)?;
        Ok(Self {
            teams_dir,
            security,
            teams: RwLock::new(HashMap::new()),
        })
    }

    /// Discover and load all team definitions from the teams directory.
    pub async fn discover(&self) -> Result<usize> {
        info!("Discovering team definitions in: {}", self.teams_dir.display());

        let mut count = 0;
        let mut teams = HashMap::new();

        let entries = match fs::read_dir(&self.teams_dir) {
            Ok(entries) => entries,
            Err(e) => {
                warn!("Failed to read teams directory: {}", e);
                return Ok(0);
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("yaml")
                && path.extension().and_then(|s| s.to_str()) != Some("yml")
            {
                continue;
            }

            match self.load_team_from_file(&path).await {
                Ok(team) => {
                    let id = team.id().to_string();
                    // Validate the team definition
                    if let Err(e) = team.validate() {
                        warn!("Invalid team definition in {:?}: {}", path, e);
                        continue;
                    }
                    debug!("Loaded team '{}' from: {:?}", id, path);
                    teams.insert(id, team);
                    count += 1;
                }
                Err(e) => {
                    warn!("Failed to load team from {:?}: {}", path, e);
                }
            }
        }

        *self.teams.write().await = teams;
        info!("Discovered {} team definition(s)", count);
        Ok(count)
    }

    /// Reload all team definitions from disk.
    pub async fn reload(&self) -> Result<usize> {
        info!("Reloading team definitions");
        self.discover().await
    }

    /// Check if a team exists in the registry.
    pub async fn contains(&self, id: &str) -> bool {
        self.teams.read().await.contains_key(id)
    }

    /// Get a team definition by ID.
    pub async fn get(&self, id: &str) -> Option<TeamDefinition> {
        self.teams.read().await.get(id).cloned()
    }

    /// List all team IDs.
    pub async fn list(&self) -> Vec<String> {
        let teams = self.teams.read().await;
        let mut ids: Vec<String> = teams.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Add or update a team definition.
    pub async fn register(&self, team: TeamDefinition) -> Result<()> {
        // Validate before registering
        team.validate()?;

        let id = team.id().to_string();
        self.teams.write().await.insert(id.clone(), team);
        info!("Registered team: {}", id);
        Ok(())
    }

    /// Remove a team from the registry.
    pub async fn unregister(&self, id: &str) -> Result<()> {
        use tokio::sync::RwLockWriteGuard;
        let mut teams: RwLockWriteGuard<'_, HashMap<String, TeamDefinition>> =
            self.teams.write().await;

        if teams.remove(id).is_none() {
            bail!("Team '{}' not found in registry", id);
        }

        info!("Unregistered team: {}", id);
        Ok(())
    }

    /// Get the teams directory path.
    #[must_use]
    pub fn teams_dir(&self) -> &PathBuf {
        &self.teams_dir
    }

    /// Load a team definition from a file.
    async fn load_team_from_file(&self, path: &Path) -> Result<TeamDefinition> {
        let content = fs::read_to_string(path)?;

        // Parse YAML
        let team: TeamDefinition = serde_yaml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse team YAML from {:?}: {}", path, e))?;

        Ok(team)
    }

    /// Save a team definition to a file.
    pub async fn save_team(&self, team: &TeamDefinition) -> Result<()> {
        // Validate before saving
        team.validate()?;

        let filename = format!("{}.yaml", team.id());
        let path = self.teams_dir.join(&filename);

        let yaml = serde_yaml::to_string(team)
            .map_err(|e| anyhow::anyhow!("Failed to serialize team: {}", e))?;

        fs::write(&path, yaml)?;
        info!("Saved team '{}' to: {}", team.id(), path.display());

        // Update registry
        self.register(team.clone()).await?;
        Ok(())
    }

    /// Delete a team definition file.
    pub async fn delete_team(&self, id: &str) -> Result<()> {
        let filename = format!("{}.yaml", id);
        let path = self.teams_dir.join(&filename);

        if !path.exists() {
            bail!("Team file not found: {}", path.display());
        }

        fs::remove_file(&path)?;
        info!("Deleted team file: {}", path.display());

        // Remove from registry
        self.unregister(id).await?;
        Ok(())
    }

    /// Get team statistics.
    pub async fn stats(&self) -> TeamStats {
        let teams = self.teams.read().await;

        let total = teams.len();
        let mut by_topology: HashMap<TeamTopologyType, usize> = HashMap::new();
        let mut by_budget_tier: HashMap<BudgetTier, usize> = HashMap::new();
        let mut by_role: HashMap<AgentRole, usize> = HashMap::new();
        let mut total_members = 0;

        for team in teams.values() {
            *by_topology.entry(team.topology_type()).or_insert(0) += 1;
            *by_budget_tier.entry(team.budget.tier).or_insert(0) += 1;
            total_members += team.members.len();
            for member in &team.members {
                *by_role.entry(member.role).or_insert(0) += 1;
            }
        }

        TeamStats {
            total,
            by_topology,
            by_budget_tier,
            by_role,
            total_members,
        }
    }
}

/// Team registry statistics.
#[derive(Debug, Clone)]
pub struct TeamStats {
    /// Total number of teams
    pub total: usize,

    /// Teams by topology
    pub by_topology: HashMap<TeamTopologyType, usize>,

    /// Teams by budget tier
    pub by_budget_tier: HashMap<BudgetTier, usize>,

    /// Team members by role
    pub by_role: HashMap<AgentRole, usize>,

    /// Total number of unique agents across all teams
    pub total_members: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_team_registry() {
        let temp_dir = TempDir::new().unwrap();
        let teams_dir = temp_dir.path().join("teams");
        fs::create_dir_all(&teams_dir).unwrap();

        let security = Arc::new(crate::security::SecurityPolicy::default());
        let registry = TeamRegistry::new(teams_dir, security).unwrap();

        // Test empty registry
        assert_eq!(registry.discover().await.unwrap(), 0);
        assert_eq!(registry.list().await.len(), 0);

        // Test adding a team
        let team = TeamDefinition {
            team: super::super::team_definition::TeamMetadata {
                id: "test-team".to_string(),
                name: "Test Team".to_string(),
                version: "1.0.0".to_string(),
                description: "A test team".to_string(),
            },
            topology: super::super::team_definition::TeamTopologyConfig {
                topology_type: super::super::team_definition::TeamTopologyType::Single,
                lead: None,
            },
            coordination: super::super::team_definition::TeamCoordination::default(),
            budget: super::super::team_definition::TeamBudget::default(),
            workload: super::super::team_definition::TeamWorkload::default(),
            degradation: super::super::team_definition::TeamDegradation::default(),
            members: vec![],
        };

        // This should fail validation (no members)
        assert!(registry.save_team(&team).await.is_err());

        // Fix the team - add a member
        let valid_team = TeamDefinition {
            team: super::super::team_definition::TeamMetadata {
                id: "test-team".to_string(),
                name: "Test Team".to_string(),
                version: "1.0.0".to_string(),
                description: "A test team".to_string(),
            },
            topology: super::super::team_definition::TeamTopologyConfig {
                topology_type: super::super::team_definition::TeamTopologyType::Single,
                lead: None,
            },
            coordination: super::super::team_definition::TeamCoordination::default(),
            budget: super::super::team_definition::TeamBudget::default(),
            workload: super::super::team_definition::TeamWorkload::default(),
            degradation: super::super::team_definition::TeamDegradation::default(),
            members: vec![super::super::team_definition::TeamMemberRef {
                agent_id: "agent1".to_string(),
                role: super::super::team_definition::AgentRole::Worker,
                max_concurrent_tasks: 1,
                capabilities: vec![],
            }],
        };

        assert!(registry.save_team(&valid_team).await.is_ok());
        assert!(registry.contains("test-team").await);

        let loaded = registry.get("test-team").await;
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().id(), "test-team");
    }
}
