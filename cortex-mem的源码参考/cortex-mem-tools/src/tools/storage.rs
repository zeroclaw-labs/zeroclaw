// Storage Tools - Store content with automatic layer generation

use crate::{MemoryOperations, Result, types::*};
use chrono::Utc;
use cortex_mem_core::{FilesystemOperations, MessageRole};
use cortex_mem_core::memory_events::{MemoryEvent, ChangeType};
use cortex_mem_core::memory_index::MemoryScope;
use std::collections::HashMap;

impl MemoryOperations {
    /// Store content with automatic L0/L1 layer generation
    ///
    /// IMPORTANT: Layer generation is now fully asynchronous to avoid blocking
    /// the agent's response. For session scope, we send LayerUpdateNeeded events
    /// which are processed by MemoryEventCoordinator in the background.
    pub async fn store(&self, args: StoreArgs) -> Result<StoreResponse> {
        // Determine storage scope: user, session, or agent
        let scope = match args.scope.as_str() {
            "user" | "session" | "agent" => args.scope.as_str(),
            _ => "session", // Default to session
        };

        // Build URI based on scope
        let uri = match scope {
            "user" => {
                // cortex://user/{user_id}/memories/YYYY-MM/DD/HH_MM_SS_id.md
                let user_id = args.user_id.as_deref().unwrap_or("default");
                let now = Utc::now();
                let year_month = now.format("%Y-%m").to_string();
                let day = now.format("%d").to_string();
                let filename = format!(
                    "{}_{}.md",
                    now.format("%H_%M_%S"),
                    uuid::Uuid::new_v4()
                        .to_string()
                        .split('-')
                        .next()
                        .unwrap_or("unknown")
                );
                format!(
                    "cortex://user/{}/memories/{}/{}/{}",
                    user_id, year_month, day, filename
                )
            }
            "agent" => {
                // cortex://agent/{agent_id}/memories/YYYY-MM/DD/HH_MM_SS_id.md
                let agent_id = args
                    .agent_id
                    .as_deref()
                    .or_else(|| {
                        if args.thread_id.is_empty() {
                            None
                        } else {
                            Some(&args.thread_id)
                        }
                    })
                    .unwrap_or("default");
                let now = Utc::now();
                let year_month = now.format("%Y-%m").to_string();
                let day = now.format("%d").to_string();
                let filename = format!(
                    "{}_{}.md",
                    now.format("%H_%M_%S"),
                    uuid::Uuid::new_v4()
                        .to_string()
                        .split('-')
                        .next()
                        .unwrap_or("unknown")
                );
                format!(
                    "cortex://agent/{}/memories/{}/{}/{}",
                    agent_id, year_month, day, filename
                )
            }
            "session" => {
                // cortex://session/{thread_id}/timeline/YYYY-MM/DD/HH_MM_SS_id.md
                let thread_id = if args.thread_id.is_empty() {
                    "default".to_string()
                } else {
                    args.thread_id.clone()
                };

                // 🔧 Fix: Release lock immediately after operations
                let message = {
                    let sm = self.session_manager.write().await;

                    // 🔧 Ensure session exists with user_id and agent_id
                    if !sm.session_exists(&thread_id).await? {
                        // 使用create_session_with_ids传入user_id和agent_id
                        sm.create_session_with_ids(
                            &thread_id,
                            args.user_id
                                .clone()
                                .or_else(|| Some(self.default_user_id.clone())),
                            args.agent_id
                                .clone()
                                .or_else(|| Some(self.default_agent_id.clone())),
                        )
                        .await?;
                    } else {
                        // 🔧 如果session已存在但缺少user_id/agent_id，更新它
                        if let Ok(mut metadata) = sm.load_session(&thread_id).await {
                            let mut needs_update = false;

                            if metadata.user_id.is_none() {
                                metadata.user_id = args
                                    .user_id
                                    .clone()
                                    .or_else(|| Some(self.default_user_id.clone()));
                                needs_update = true;
                            }
                            if metadata.agent_id.is_none() {
                                metadata.agent_id = args
                                    .agent_id
                                    .clone()
                                    .or_else(|| Some(self.default_agent_id.clone()));
                                needs_update = true;
                            }

                            if needs_update {
                                let _ = sm.update_session(&metadata).await;
                            }
                        }
                    }

                    // 使用add_message()发布事件，而不是直接调用save_message()
                    sm.add_message(
                        &thread_id,
                        MessageRole::User, // 默认使用User角色
                        args.content.clone(),
                    )
                    .await?
                }; // Lock is released here

                // 返回消息URI
                let year_month = message.timestamp.format("%Y-%m").to_string();
                let day = message.timestamp.format("%d").to_string();
                let filename = format!(
                    "{}_{}.md",
                    message.timestamp.format("%H_%M_%S"),
                    &message.id[..8]
                );
                format!(
                    "cortex://session/{}/timeline/{}/{}/{}",
                    thread_id, year_month, day, filename
                )
            }
            _ => unreachable!(),
        };

        // For user and agent scope, directly write to filesystem
        if scope == "user" || scope == "agent" {
            self.filesystem.write(&uri, &args.content).await?;
        }

        // 🔧 Layer generation is now FULLY ASYNCHRONOUS
        // We send events to MemoryEventCoordinator which processes them in background
        // This prevents blocking the agent's response
        
        let layers_generated = HashMap::new();
        
        if args.auto_generate_layers.unwrap_or(true) {
            match scope {
                "user" => {
                    // Send LayerUpdateNeeded event for user scope
                    if let Some(ref tx) = self.memory_event_tx {
                        let user_id = args.user_id.clone().unwrap_or_else(|| self.default_user_id.clone());
                        let parent_dir = uri.rsplit_once('/')
                            .map(|(dir, _)| dir.to_string())
                            .unwrap_or_else(|| uri.clone());
                        
                        let _ = tx.send(MemoryEvent::LayerUpdateNeeded {
                            scope: MemoryScope::User,
                            owner_id: user_id,
                            directory_uri: parent_dir,
                            change_type: ChangeType::Add,
                            changed_file: uri.clone(),
                        });
                        tracing::debug!("📤 Sent LayerUpdateNeeded event for user scope");
                    } else {
                        // Fallback: synchronous generation (should not happen in production)
                        tracing::warn!("⚠️ memory_event_tx not available, falling back to sync generation");
                        if let Err(e) = self.layer_manager.generate_all_layers(&uri, &args.content).await {
                            tracing::warn!("Failed to generate layers for {}: {}", uri, e);
                        }
                    }
                }
                "agent" => {
                    // Send LayerUpdateNeeded event for agent scope
                    if let Some(ref tx) = self.memory_event_tx {
                        let agent_id = args.agent_id.clone()
                            .or_else(|| Some(args.thread_id.clone()))
                            .unwrap_or_else(|| self.default_agent_id.clone());
                        let parent_dir = uri.rsplit_once('/')
                            .map(|(dir, _)| dir.to_string())
                            .unwrap_or_else(|| uri.clone());
                        
                        let _ = tx.send(MemoryEvent::LayerUpdateNeeded {
                            scope: MemoryScope::Agent,
                            owner_id: agent_id,
                            directory_uri: parent_dir,
                            change_type: ChangeType::Add,
                            changed_file: uri.clone(),
                        });
                        tracing::debug!("📤 Sent LayerUpdateNeeded event for agent scope");
                    } else {
                        tracing::warn!("⚠️ memory_event_tx not available, falling back to sync generation");
                        if let Err(e) = self.layer_manager.generate_all_layers(&uri, &args.content).await {
                            tracing::warn!("Failed to generate layers for {}: {}", uri, e);
                        }
                    }
                }
                "session" => {
                    // Session scope: Send LayerUpdateNeeded for the timeline directory
                    // Layer generation is deferred to session close for efficiency
                    // But we can optionally trigger incremental updates here
                    if let Some(ref tx) = self.memory_event_tx {
                        let thread_id = if args.thread_id.is_empty() {
                            "default".to_string()
                        } else {
                            args.thread_id.clone()
                        };
                        let parent_dir = uri.rsplit_once('/')
                            .map(|(dir, _)| dir.to_string())
                            .unwrap_or_else(|| uri.clone());
                        
                        let _ = tx.send(MemoryEvent::LayerUpdateNeeded {
                            scope: MemoryScope::Session,
                            owner_id: thread_id,
                            directory_uri: parent_dir,
                            change_type: ChangeType::Add,
                            changed_file: uri.clone(),
                        });
                        tracing::debug!("📤 Sent LayerUpdateNeeded event for session scope");
                    }
                    // Note: Session-level layers are primarily generated on session close
                    // This event enables optional incremental updates
                }
                _ => {}
            }
        }

        Ok(StoreResponse {
            uri,
            layers_generated,
            success: true,
        })
    }
}
