//! Incremental Memory Updater Module
//!
//! Handles incremental updates to memories with version tracking.
//! Supports create, update, delete operations with proper deduplication.

use crate::filesystem::{CortexFilesystem, FilesystemOperations};
use crate::llm::LLMClient;
use crate::memory_index::{MemoryMetadata, MemoryScope, MemoryType, MemoryUpdateResult};
use crate::memory_index_manager::MemoryIndexManager;
use crate::memory_events::{DeleteReason, MemoryEvent};
use crate::session::extraction::{
    CaseMemory, EntityMemory, EventMemory, ExtractedMemories, GoalMemory,
    PersonalInfoMemory, PreferenceMemory, RelationshipMemory, WorkHistoryMemory,
};
use crate::Result;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info};

/// Incremental Memory Updater
///
/// Handles incremental updates to user and agent memories.
/// Emits events for each operation to trigger cascading updates.
pub struct IncrementalMemoryUpdater {
    filesystem: Arc<CortexFilesystem>,
    index_manager: Arc<MemoryIndexManager>,
    /// LLM client for future content comparison and merge features
    #[allow(dead_code)]
    llm_client: Arc<dyn LLMClient>,
    event_tx: mpsc::UnboundedSender<MemoryEvent>,
}

impl IncrementalMemoryUpdater {
    /// Create a new incremental memory updater
    pub fn new(
        filesystem: Arc<CortexFilesystem>,
        index_manager: Arc<MemoryIndexManager>,
        llm_client: Arc<dyn LLMClient>,
        event_tx: mpsc::UnboundedSender<MemoryEvent>,
    ) -> Self {
        Self {
            filesystem,
            index_manager,
            llm_client,
            event_tx,
        }
    }

    /// Update memories from extracted session data
    ///
    /// This is the main entry point for memory updates during session close.
    /// It handles creation, update, and deletion with proper event emission.
    pub async fn update_memories(
        &self,
        user_id: &str,
        agent_id: &str,
        session_id: &str,
        extracted: &ExtractedMemories,
    ) -> Result<MemoryUpdateResult> {
        let mut result = MemoryUpdateResult::default();
        
        // Process each memory type
        self.process_preferences(&mut result, user_id, session_id, &extracted.preferences).await?;
        self.process_entities(&mut result, user_id, session_id, &extracted.entities).await?;
        self.process_events(&mut result, user_id, session_id, &extracted.events).await?;
        self.process_cases(&mut result, agent_id, session_id, &extracted.cases).await?;
        self.process_personal_info(&mut result, user_id, session_id, &extracted.personal_info).await?;
        self.process_work_history(&mut result, user_id, session_id, &extracted.work_history).await?;
        self.process_relationships(&mut result, user_id, session_id, &extracted.relationships).await?;
        self.process_goals(&mut result, user_id, session_id, &extracted.goals).await?;
        
        // Record session extraction summary
        self.index_manager.record_session_extraction(
            &MemoryScope::User,
            user_id,
            session_id,
            result.created_ids.clone(),
            result.updated_ids.clone(),
        ).await?;
        
        info!(
            "Memory update complete for session {}: {} created, {} updated, {} deleted",
            session_id, result.created, result.updated, result.deleted
        );
        
        Ok(result)
    }

    /// Process preference memories
    async fn process_preferences(
        &self,
        result: &mut MemoryUpdateResult,
        user_id: &str,
        session_id: &str,
        preferences: &[PreferenceMemory],
    ) -> Result<()> {
        for pref in preferences {
            let key = &pref.topic;
            let existing = self.index_manager
                .find_matching_memory(&MemoryScope::User, user_id, &MemoryType::Preference, key)
                .await?;
            
            let content = self.format_preference_content(pref);
            let content_hash = MemoryIndexManager::calculate_content_hash(&content);
            let content_summary = MemoryIndexManager::generate_content_summary(&content, 200);
            
            match existing {
                Some(existing_meta) => {
                    // Check if update is needed
                    if self.should_update(&existing_meta, pref.confidence, &content_hash, &content_summary).await? {
                        // Update existing memory
                        self.update_memory(
                            result,
                            user_id,
                            session_id,
                            existing_meta,
                            content,
                            content_hash,
                            content_summary,
                            pref.confidence,
                        ).await?;
                    }
                }
                None => {
                    // Create new memory
                    self.create_preference(result, user_id, session_id, pref, content, content_hash, content_summary).await?;
                }
            }
        }
        Ok(())
    }

    /// Process entity memories
    async fn process_entities(
        &self,
        result: &mut MemoryUpdateResult,
        user_id: &str,
        session_id: &str,
        entities: &[EntityMemory],
    ) -> Result<()> {
        for entity in entities {
            let key = &entity.name;
            let existing = self.index_manager
                .find_matching_memory(&MemoryScope::User, user_id, &MemoryType::Entity, key)
                .await?;
            
            let content = self.format_entity_content(entity);
            let content_hash = MemoryIndexManager::calculate_content_hash(&content);
            let content_summary = MemoryIndexManager::generate_content_summary(&content, 200);
            
            match existing {
                Some(existing_meta) => {
                    if self.should_update_entity(&existing_meta, entity, &content_hash).await? {
                        self.update_memory(
                            result,
                            user_id,
                            session_id,
                            existing_meta,
                            content,
                            content_hash,
                            content_summary,
                            0.9,
                        ).await?;
                    }
                }
                None => {
                    self.create_entity(result, user_id, session_id, entity, content, content_hash).await?;
                }
            }
        }
        Ok(())
    }

    /// Process event memories
    async fn process_events(
        &self,
        result: &mut MemoryUpdateResult,
        user_id: &str,
        session_id: &str,
        events: &[EventMemory],
    ) -> Result<()> {
        for event in events {
            let key = &event.title;
            let existing = self.index_manager
                .find_matching_memory(&MemoryScope::User, user_id, &MemoryType::Event, key)
                .await?;
            
            let content = self.format_event_content(event);
            let content_hash = MemoryIndexManager::calculate_content_hash(&content);
            let content_summary = MemoryIndexManager::generate_content_summary(&content, 200);
            
            match existing {
                Some(existing_meta) => {
                    if self.should_update(&existing_meta, 0.8, &content_hash, &content_summary).await? {
                        self.update_memory(
                            result,
                            user_id,
                            session_id,
                            existing_meta,
                            content,
                            content_hash,
                            content_summary,
                            0.8,
                        ).await?;
                    }
                }
                None => {
                    self.create_event(result, user_id, session_id, event, content, content_hash).await?;
                }
            }
        }
        Ok(())
    }

    /// Process agent case memories
    async fn process_cases(
        &self,
        result: &mut MemoryUpdateResult,
        agent_id: &str,
        session_id: &str,
        cases: &[CaseMemory],
    ) -> Result<()> {
        for case in cases {
            let key = &case.title;
            let existing = self.index_manager
                .find_matching_memory(&MemoryScope::Agent, agent_id, &MemoryType::Case, key)
                .await?;
            
            let content = self.format_case_content(case);
            let content_hash = MemoryIndexManager::calculate_content_hash(&content);
            let content_summary = MemoryIndexManager::generate_content_summary(&content, 200);
            
            match existing {
                Some(existing_meta) => {
                    if self.should_update(&existing_meta, 0.8, &content_hash, &content_summary).await? {
                        self.update_memory_agent(
                            result,
                            agent_id,
                            session_id,
                            existing_meta,
                            content,
                            content_hash,
                            content_summary,
                        ).await?;
                    }
                }
                None => {
                    self.create_case(result, agent_id, session_id, case, content, content_hash).await?;
                }
            }
        }
        Ok(())
    }

    /// Process personal info memories
    async fn process_personal_info(
        &self,
        result: &mut MemoryUpdateResult,
        user_id: &str,
        session_id: &str,
        personal_info: &[PersonalInfoMemory],
    ) -> Result<()> {
        for info in personal_info {
            let key = &info.category;
            let existing = self.index_manager
                .find_matching_memory(&MemoryScope::User, user_id, &MemoryType::PersonalInfo, key)
                .await?;
            
            let content = self.format_personal_info_content(info);
            let content_hash = MemoryIndexManager::calculate_content_hash(&content);
            let content_summary = MemoryIndexManager::generate_content_summary(&content, 200);
            
            match existing {
                Some(existing_meta) => {
                    if self.should_update(&existing_meta, info.confidence, &content_hash, &content_summary).await? {
                        self.update_memory(
                            result,
                            user_id,
                            session_id,
                            existing_meta,
                            content,
                            content_hash,
                            content_summary,
                            info.confidence,
                        ).await?;
                    }
                }
                None => {
                    self.create_personal_info(result, user_id, session_id, info, content, content_hash).await?;
                }
            }
        }
        Ok(())
    }

    /// Process work history memories
    async fn process_work_history(
        &self,
        result: &mut MemoryUpdateResult,
        user_id: &str,
        session_id: &str,
        work_history: &[WorkHistoryMemory],
    ) -> Result<()> {
        for work in work_history {
            let key = format!("{}_{}", work.company, work.role);
            let existing = self.index_manager
                .find_matching_memory(&MemoryScope::User, user_id, &MemoryType::WorkHistory, &key)
                .await?;
            
            let content = self.format_work_history_content(work);
            let content_hash = MemoryIndexManager::calculate_content_hash(&content);
            let content_summary = MemoryIndexManager::generate_content_summary(&content, 200);
            
            match existing {
                Some(existing_meta) => {
                    if self.should_update(&existing_meta, work.confidence, &content_hash, &content_summary).await? {
                        self.update_memory(
                            result,
                            user_id,
                            session_id,
                            existing_meta,
                            content,
                            content_hash,
                            content_summary,
                            work.confidence,
                        ).await?;
                    }
                }
                None => {
                    self.create_work_history(result, user_id, session_id, work, content, content_hash).await?;
                }
            }
        }
        Ok(())
    }

    /// Process relationship memories
    async fn process_relationships(
        &self,
        result: &mut MemoryUpdateResult,
        user_id: &str,
        session_id: &str,
        relationships: &[RelationshipMemory],
    ) -> Result<()> {
        for rel in relationships {
            let key = &rel.person;
            let existing = self.index_manager
                .find_matching_memory(&MemoryScope::User, user_id, &MemoryType::Relationship, key)
                .await?;
            
            let content = self.format_relationship_content(rel);
            let content_hash = MemoryIndexManager::calculate_content_hash(&content);
            let content_summary = MemoryIndexManager::generate_content_summary(&content, 200);
            
            match existing {
                Some(existing_meta) => {
                    if self.should_update(&existing_meta, rel.confidence, &content_hash, &content_summary).await? {
                        self.update_memory(
                            result,
                            user_id,
                            session_id,
                            existing_meta,
                            content,
                            content_hash,
                            content_summary,
                            rel.confidence,
                        ).await?;
                    }
                }
                None => {
                    self.create_relationship(result, user_id, session_id, rel, content, content_hash).await?;
                }
            }
        }
        Ok(())
    }

    /// Process goal memories
    async fn process_goals(
        &self,
        result: &mut MemoryUpdateResult,
        user_id: &str,
        session_id: &str,
        goals: &[GoalMemory],
    ) -> Result<()> {
        for goal in goals {
            let key = &goal.goal;
            let existing = self.index_manager
                .find_matching_memory(&MemoryScope::User, user_id, &MemoryType::Goal, key)
                .await?;
            
            let content = self.format_goal_content(goal);
            let content_hash = MemoryIndexManager::calculate_content_hash(&content);
            let content_summary = MemoryIndexManager::generate_content_summary(&content, 200);
            
            match existing {
                Some(existing_meta) => {
                    if self.should_update(&existing_meta, goal.confidence, &content_hash, &content_summary).await? {
                        self.update_memory(
                            result,
                            user_id,
                            session_id,
                            existing_meta,
                            content,
                            content_hash,
                            content_summary,
                            goal.confidence,
                        ).await?;
                    }
                }
                None => {
                    self.create_goal(result, user_id, session_id, goal, content, content_hash).await?;
                }
            }
        }
        Ok(())
    }

    /// Check if an existing memory should be updated
    async fn should_update(
        &self,
        existing: &MemoryMetadata,
        new_confidence: f32,
        new_hash: &str,
        new_summary: &str,
    ) -> Result<bool> {
        // Update if new confidence is significantly higher
        if new_confidence > existing.confidence + 0.1 {
            return Ok(true);
        }
        
        // Update if content changed
        if MemoryIndexManager::content_changed(
            &existing.content_hash,
            new_hash,
            &existing.content_summary,
            new_summary,
        ) {
            return Ok(true);
        }
        
        Ok(false)
    }

    /// Check if entity should be updated (with context comparison)
    async fn should_update_entity(
        &self,
        existing: &MemoryMetadata,
        _new_entity: &EntityMemory,
        new_hash: &str,
    ) -> Result<bool> {
        // Always update if content hash changed
        if existing.content_hash != new_hash {
            return Ok(true);
        }
        
        // Update if entity type or description differs significantly
        // This is a simplified check - can be enhanced with LLM comparison
        Ok(false)
    }

    /// Update an existing memory
    async fn update_memory(
        &self,
        result: &mut MemoryUpdateResult,
        user_id: &str,
        session_id: &str,
        existing: MemoryMetadata,
        content: String,
        content_hash: String,
        content_summary: String,
        confidence: f32,
    ) -> Result<()> {
        let file_uri = format!("cortex://user/{}/{}", user_id, existing.file);
        let memory_id = existing.id.clone();
        let old_hash = existing.content_hash.clone();
        let new_hash = content_hash.clone();
        
        // Write updated content
        let timestamped_content = self.add_timestamp(&content);
        self.filesystem.write(&file_uri, &timestamped_content).await?;
        
        // Update metadata
        let mut updated_meta = existing.clone();
        updated_meta.update(content_hash, session_id, confidence, content_summary);
        
        // Update index
        self.index_manager.upsert_memory(&MemoryScope::User, user_id, updated_meta).await?;
        
        // Emit event
        let _ = self.event_tx.send(MemoryEvent::MemoryUpdated {
            scope: MemoryScope::User,
            owner_id: user_id.to_string(),
            memory_id: memory_id.clone(),
            memory_type: existing.memory_type.clone(),
            key: existing.key.clone(),
            source_session: session_id.to_string(),
            file_uri: file_uri.clone(),
            old_content_hash: old_hash,
            new_content_hash: new_hash,
        });
        
        result.updated += 1;
        result.updated_ids.push(memory_id.clone());
        
        debug!("Updated memory {} for user {}", memory_id, user_id);
        Ok(())
    }

    /// Update agent memory
    async fn update_memory_agent(
        &self,
        result: &mut MemoryUpdateResult,
        agent_id: &str,
        session_id: &str,
        existing: MemoryMetadata,
        content: String,
        content_hash: String,
        content_summary: String,
    ) -> Result<()> {
        let file_uri = format!("cortex://agent/{}/{}", agent_id, existing.file);
        let memory_id = existing.id.clone();
        let old_hash = existing.content_hash.clone();
        let new_hash = content_hash.clone();
        
        // Write updated content
        let timestamped_content = self.add_timestamp(&content);
        self.filesystem.write(&file_uri, &timestamped_content).await?;
        
        // Update metadata
        let mut updated_meta = existing.clone();
        updated_meta.update(content_hash, session_id, 0.9, content_summary);
        
        // Update index
        self.index_manager.upsert_memory(&MemoryScope::Agent, agent_id, updated_meta).await?;
        
        // Emit event
        let _ = self.event_tx.send(MemoryEvent::MemoryUpdated {
            scope: MemoryScope::Agent,
            owner_id: agent_id.to_string(),
            memory_id: memory_id.clone(),
            memory_type: MemoryType::Case,
            key: existing.key.clone(),
            source_session: session_id.to_string(),
            file_uri: file_uri.clone(),
            old_content_hash: old_hash,
            new_content_hash: new_hash,
        });
        
        result.updated += 1;
        result.updated_ids.push(memory_id.clone());
        
        Ok(())
    }

    // Create methods for each memory type
    async fn create_preference(
        &self,
        result: &mut MemoryUpdateResult,
        user_id: &str,
        session_id: &str,
        pref: &PreferenceMemory,
        content: String,
        content_hash: String,
        content_summary: String,
    ) -> Result<()> {
        let memory_id = format!("pref_{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap());
        let file_path = format!("preferences/{}.md", memory_id);
        let file_uri = format!("cortex://user/{}/{}", user_id, file_path);
        
        // Write content
        let timestamped_content = self.add_timestamp(&content);
        self.filesystem.write(&file_uri, &timestamped_content).await?;
        
        // Create metadata
        let metadata = MemoryMetadata::new(
            memory_id.clone(),
            file_path,
            MemoryType::Preference,
            pref.topic.clone(),
            content_hash,
            session_id,
            pref.confidence,
            content_summary,
        );
        
        // Update index
        self.index_manager.upsert_memory(&MemoryScope::User, user_id, metadata).await?;
        
        // Emit event
        let _ = self.event_tx.send(MemoryEvent::MemoryCreated {
            scope: MemoryScope::User,
            owner_id: user_id.to_string(),
            memory_id: memory_id.clone(),
            memory_type: MemoryType::Preference,
            key: pref.topic.clone(),
            source_session: session_id.to_string(),
            file_uri,
        });
        
        result.created += 1;
        result.created_ids.push(memory_id);
        
        Ok(())
    }

    async fn create_entity(
        &self,
        result: &mut MemoryUpdateResult,
        user_id: &str,
        session_id: &str,
        entity: &EntityMemory,
        content: String,
        content_hash: String,
    ) -> Result<()> {
        let memory_id = format!("entity_{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap());
        let file_path = format!("entities/{}.md", memory_id);
        let file_uri = format!("cortex://user/{}/{}", user_id, file_path);
        let content_summary = MemoryIndexManager::generate_content_summary(&content, 200);
        
        let timestamped_content = self.add_timestamp(&content);
        self.filesystem.write(&file_uri, &timestamped_content).await?;
        
        let metadata = MemoryMetadata::new(
            memory_id.clone(),
            file_path,
            MemoryType::Entity,
            entity.name.clone(),
            content_hash,
            session_id,
            0.9,
            content_summary,
        );
        
        self.index_manager.upsert_memory(&MemoryScope::User, user_id, metadata).await?;
        
        let _ = self.event_tx.send(MemoryEvent::MemoryCreated {
            scope: MemoryScope::User,
            owner_id: user_id.to_string(),
            memory_id: memory_id.clone(),
            memory_type: MemoryType::Entity,
            key: entity.name.clone(),
            source_session: session_id.to_string(),
            file_uri,
        });
        
        result.created += 1;
        result.created_ids.push(memory_id);
        
        Ok(())
    }

    async fn create_event(
        &self,
        result: &mut MemoryUpdateResult,
        user_id: &str,
        session_id: &str,
        event: &EventMemory,
        content: String,
        content_hash: String,
    ) -> Result<()> {
        let memory_id = format!("event_{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap());
        let file_path = format!("events/{}.md", memory_id);
        let file_uri = format!("cortex://user/{}/{}", user_id, file_path);
        let content_summary = MemoryIndexManager::generate_content_summary(&content, 200);
        
        let timestamped_content = self.add_timestamp(&content);
        self.filesystem.write(&file_uri, &timestamped_content).await?;
        
        let metadata = MemoryMetadata::new(
            memory_id.clone(),
            file_path,
            MemoryType::Event,
            event.title.clone(),
            content_hash,
            session_id,
            0.8,
            content_summary,
        );
        
        self.index_manager.upsert_memory(&MemoryScope::User, user_id, metadata).await?;
        
        let _ = self.event_tx.send(MemoryEvent::MemoryCreated {
            scope: MemoryScope::User,
            owner_id: user_id.to_string(),
            memory_id: memory_id.clone(),
            memory_type: MemoryType::Event,
            key: event.title.clone(),
            source_session: session_id.to_string(),
            file_uri,
        });
        
        result.created += 1;
        result.created_ids.push(memory_id);
        
        Ok(())
    }

    async fn create_case(
        &self,
        result: &mut MemoryUpdateResult,
        agent_id: &str,
        session_id: &str,
        case: &CaseMemory,
        content: String,
        content_hash: String,
    ) -> Result<()> {
        let memory_id = format!("case_{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap());
        let file_path = format!("cases/{}.md", memory_id);
        let file_uri = format!("cortex://agent/{}/{}", agent_id, file_path);
        let content_summary = MemoryIndexManager::generate_content_summary(&content, 200);
        
        let timestamped_content = self.add_timestamp(&content);
        self.filesystem.write(&file_uri, &timestamped_content).await?;
        
        let metadata = MemoryMetadata::new(
            memory_id.clone(),
            file_path,
            MemoryType::Case,
            case.title.clone(),
            content_hash,
            session_id,
            0.9,
            content_summary,
        );
        
        self.index_manager.upsert_memory(&MemoryScope::Agent, agent_id, metadata).await?;
        
        let _ = self.event_tx.send(MemoryEvent::MemoryCreated {
            scope: MemoryScope::Agent,
            owner_id: agent_id.to_string(),
            memory_id: memory_id.clone(),
            memory_type: MemoryType::Case,
            key: case.title.clone(),
            source_session: session_id.to_string(),
            file_uri,
        });
        
        result.created += 1;
        result.created_ids.push(memory_id);
        
        Ok(())
    }

    async fn create_personal_info(
        &self,
        result: &mut MemoryUpdateResult,
        user_id: &str,
        session_id: &str,
        info: &PersonalInfoMemory,
        content: String,
        content_hash: String,
    ) -> Result<()> {
        let memory_id = format!("info_{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap());
        let file_path = format!("personal_info/{}.md", memory_id);
        let file_uri = format!("cortex://user/{}/{}", user_id, file_path);
        let content_summary = MemoryIndexManager::generate_content_summary(&content, 200);
        
        let timestamped_content = self.add_timestamp(&content);
        self.filesystem.write(&file_uri, &timestamped_content).await?;
        
        let metadata = MemoryMetadata::new(
            memory_id.clone(),
            file_path,
            MemoryType::PersonalInfo,
            info.category.clone(),
            content_hash,
            session_id,
            info.confidence,
            content_summary,
        );
        
        self.index_manager.upsert_memory(&MemoryScope::User, user_id, metadata).await?;
        
        let _ = self.event_tx.send(MemoryEvent::MemoryCreated {
            scope: MemoryScope::User,
            owner_id: user_id.to_string(),
            memory_id: memory_id.clone(),
            memory_type: MemoryType::PersonalInfo,
            key: info.category.clone(),
            source_session: session_id.to_string(),
            file_uri,
        });
        
        result.created += 1;
        result.created_ids.push(memory_id);
        
        Ok(())
    }

    async fn create_work_history(
        &self,
        result: &mut MemoryUpdateResult,
        user_id: &str,
        session_id: &str,
        work: &WorkHistoryMemory,
        content: String,
        content_hash: String,
    ) -> Result<()> {
        let memory_id = format!("work_{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap());
        let file_path = format!("work_history/{}.md", memory_id);
        let file_uri = format!("cortex://user/{}/{}", user_id, file_path);
        let content_summary = MemoryIndexManager::generate_content_summary(&content, 200);
        
        let timestamped_content = self.add_timestamp(&content);
        self.filesystem.write(&file_uri, &timestamped_content).await?;
        
        let key = format!("{}_{}", work.company, work.role);
        let metadata = MemoryMetadata::new(
            memory_id.clone(),
            file_path,
            MemoryType::WorkHistory,
            key,
            content_hash,
            session_id,
            work.confidence,
            content_summary,
        );
        
        self.index_manager.upsert_memory(&MemoryScope::User, user_id, metadata).await?;
        
        let _ = self.event_tx.send(MemoryEvent::MemoryCreated {
            scope: MemoryScope::User,
            owner_id: user_id.to_string(),
            memory_id: memory_id.clone(),
            memory_type: MemoryType::WorkHistory,
            key: format!("{}_{}", work.company, work.role),
            source_session: session_id.to_string(),
            file_uri,
        });
        
        result.created += 1;
        result.created_ids.push(memory_id);
        
        Ok(())
    }

    async fn create_relationship(
        &self,
        result: &mut MemoryUpdateResult,
        user_id: &str,
        session_id: &str,
        rel: &RelationshipMemory,
        content: String,
        content_hash: String,
    ) -> Result<()> {
        let memory_id = format!("rel_{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap());
        let file_path = format!("relationships/{}.md", memory_id);
        let file_uri = format!("cortex://user/{}/{}", user_id, file_path);
        let content_summary = MemoryIndexManager::generate_content_summary(&content, 200);
        
        let timestamped_content = self.add_timestamp(&content);
        self.filesystem.write(&file_uri, &timestamped_content).await?;
        
        let metadata = MemoryMetadata::new(
            memory_id.clone(),
            file_path,
            MemoryType::Relationship,
            rel.person.clone(),
            content_hash,
            session_id,
            rel.confidence,
            content_summary,
        );
        
        self.index_manager.upsert_memory(&MemoryScope::User, user_id, metadata).await?;
        
        let _ = self.event_tx.send(MemoryEvent::MemoryCreated {
            scope: MemoryScope::User,
            owner_id: user_id.to_string(),
            memory_id: memory_id.clone(),
            memory_type: MemoryType::Relationship,
            key: rel.person.clone(),
            source_session: session_id.to_string(),
            file_uri,
        });
        
        result.created += 1;
        result.created_ids.push(memory_id);
        
        Ok(())
    }

    async fn create_goal(
        &self,
        result: &mut MemoryUpdateResult,
        user_id: &str,
        session_id: &str,
        goal: &GoalMemory,
        content: String,
        content_hash: String,
    ) -> Result<()> {
        let memory_id = format!("goal_{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap());
        let file_path = format!("goals/{}.md", memory_id);
        let file_uri = format!("cortex://user/{}/{}", user_id, file_path);
        let content_summary = MemoryIndexManager::generate_content_summary(&content, 200);
        
        let timestamped_content = self.add_timestamp(&content);
        self.filesystem.write(&file_uri, &timestamped_content).await?;
        
        let metadata = MemoryMetadata::new(
            memory_id.clone(),
            file_path,
            MemoryType::Goal,
            goal.goal.clone(),
            content_hash,
            session_id,
            goal.confidence,
            content_summary,
        );
        
        self.index_manager.upsert_memory(&MemoryScope::User, user_id, metadata).await?;
        
        let _ = self.event_tx.send(MemoryEvent::MemoryCreated {
            scope: MemoryScope::User,
            owner_id: user_id.to_string(),
            memory_id: memory_id.clone(),
            memory_type: MemoryType::Goal,
            key: goal.goal.clone(),
            source_session: session_id.to_string(),
            file_uri,
        });
        
        result.created += 1;
        result.created_ids.push(memory_id);
        
        Ok(())
    }

    // Content formatting methods
    fn format_preference_content(&self, pref: &PreferenceMemory) -> String {
        format!(
            "# {}\n\n{}\n\n**Confidence**: {:.2}",
            pref.topic,
            pref.preference,
            pref.confidence
        )
    }

    fn format_entity_content(&self, entity: &EntityMemory) -> String {
        format!(
            "# {}\n\n**Type**: {}\n\n**Description**: {}\n\n**Context**: {}",
            entity.name,
            entity.entity_type,
            entity.description,
            entity.context
        )
    }

    fn format_event_content(&self, event: &EventMemory) -> String {
        let timestamp = event.timestamp.as_deref().unwrap_or("N/A");
        format!(
            "# {}\n\n**Type**: {}\n\n**Summary**: {}\n\n**Timestamp**: {}",
            event.title,
            event.event_type,
            event.summary,
            timestamp
        )
    }

    fn format_case_content(&self, case: &CaseMemory) -> String {
        let lessons = case
            .lessons_learned
            .iter()
            .map(|l| format!("- {}", l))
            .collect::<Vec<_>>()
            .join("\n");
        
        format!(
            "# {}\n\n## Problem\n\n{}\n\n## Solution\n\n{}\n\n## Lessons Learned\n\n{}",
            case.title,
            case.problem,
            case.solution,
            lessons
        )
    }

    fn format_personal_info_content(&self, info: &PersonalInfoMemory) -> String {
        format!(
            "# {}\n\n{}\n\n**Confidence**: {:.2}",
            info.category,
            info.content,
            info.confidence
        )
    }

    fn format_work_history_content(&self, work: &WorkHistoryMemory) -> String {
        let duration = work.duration.as_deref().unwrap_or("N/A");
        format!(
            "# {} - {}\n\n**Duration**: {}\n\n**Description**: {}\n\n**Confidence**: {:.2}",
            work.company,
            work.role,
            duration,
            work.description,
            work.confidence
        )
    }

    fn format_relationship_content(&self, rel: &RelationshipMemory) -> String {
        format!(
            "# {}\n\n**Type**: {}\n\n**Context**: {}\n\n**Confidence**: {:.2}",
            rel.person,
            rel.relation_type,
            rel.context,
            rel.confidence
        )
    }

    fn format_goal_content(&self, goal: &GoalMemory) -> String {
        let timeline = goal.timeline.as_deref().unwrap_or("未指定");
        format!(
            "# {}\n\n**Category**: {}\n\n**Timeline**: {}\n\n**Confidence**: {:.2}",
            goal.goal,
            goal.category,
            timeline,
            goal.confidence
        )
    }

    fn add_timestamp(&self, content: &str) -> String {
        let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
        format!("{}\n\n**Added**: {}", content, timestamp)
    }

    /// Delete a memory
    pub async fn delete_memory(
        &self,
        scope: &MemoryScope,
        owner_id: &str,
        memory_id: &str,
        reason: DeleteReason,
    ) -> Result<bool> {
        // Get metadata first
        let index = self.index_manager.load_index(scope.clone(), owner_id.to_string()).await?;
        
        if let Some(metadata) = index.memories.get(memory_id).cloned() {
            let file_uri = format!(
                "cortex://{}/{}/{}",
                match scope {
                    MemoryScope::User => "user",
                    MemoryScope::Agent => "agent",
                    MemoryScope::Session => "session",
                    MemoryScope::Resources => "resources",
                },
                owner_id,
                metadata.file
            );
            
            // Delete file
            if self.filesystem.exists(&file_uri).await? {
                self.filesystem.delete(&file_uri).await?;
            }
            
            // Remove from index
            self.index_manager.remove_memory(scope, owner_id, memory_id).await?;
            
            // Emit event
            let _ = self.event_tx.send(MemoryEvent::MemoryDeleted {
                scope: scope.clone(),
                owner_id: owner_id.to_string(),
                memory_id: memory_id.to_string(),
                memory_type: metadata.memory_type,
                file_uri,
                reason,
            });
            
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
