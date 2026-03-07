use crate::{layers::manager::LayerManager, CortexFilesystem, FilesystemOperations, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Message role in a conversation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

/// A message in a session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: MessageRole,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub created_at: DateTime<Utc>, // Alias for timestamp, for compatibility
    pub metadata: Option<serde_json::Value>,
}

impl Message {
    /// Create a new message
    pub fn new(role: MessageRole, content: impl Into<String>) -> Self {
        let timestamp = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            role,
            content: content.into(),
            timestamp,
            created_at: timestamp,
            metadata: None,
        }
    }

    /// Create a user message
    pub fn user(content: impl Into<String>) -> Self {
        Self::new(MessageRole::User, content)
    }

    /// Create an assistant message
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(MessageRole::Assistant, content)
    }

    /// Create a system message
    pub fn system(content: impl Into<String>) -> Self {
        Self::new(MessageRole::System, content)
    }

    /// Add metadata
    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Convert to markdown format
    pub fn to_markdown(&self) -> String {
        let role_emoji = match self.role {
            MessageRole::User => "👤",
            MessageRole::Assistant => "🤖",
            MessageRole::System => "⚙️",
        };

        let timestamp = self.timestamp.format("%Y-%m-%d %H:%M:%S UTC");

        let mut md = format!(
            "# {} {:?}\n\n**ID**: `{}`  \n**Timestamp**: {}\n\n",
            role_emoji, self.role, self.id, timestamp
        );

        md.push_str("## Content\n\n");
        md.push_str(&self.content);
        md.push_str("\n\n");

        if let Some(ref metadata) = self.metadata {
            md.push_str("## Metadata\n\n");
            md.push_str("```json\n");
            md.push_str(&serde_json::to_string_pretty(metadata).unwrap_or_default());
            md.push_str("\n```\n");
        }

        md
    }
}

/// Message storage interface
pub struct MessageStorage {
    filesystem: Arc<CortexFilesystem>,
}

impl MessageStorage {
    pub fn new(filesystem: Arc<CortexFilesystem>) -> Self {
        Self { filesystem }
    }

    /// Save a message to the timeline
    ///
    /// URI format: cortex://session/{thread_id}/timeline/{YYYY-MM}/{DD}/{HH_MM_SS}_{message_id}.md
    ///
    /// Note: L0 and L1 layer generation is handled separately by the LayerManager
    /// to avoid coupling message storage with LLM operations
    pub async fn save_message(&self, thread_id: &str, message: &Message) -> Result<String> {
        let timestamp = message.timestamp;

        // Build timeline path: YYYY-MM/DD/HH_MM_SS_id.md
        let year_month = timestamp.format("%Y-%m").to_string();
        let day = timestamp.format("%d").to_string();
        let filename = format!(
            "{}_{}.md",
            timestamp.format("%H_%M_%S"),
            &message.id[..8] // Use first 8 chars of UUID
        );

        let uri = format!(
            "cortex://session/{}/timeline/{}/{}/{}",
            thread_id, year_month, day, filename
        );

        // Convert message to markdown
        let content = message.to_markdown();

        // Write to filesystem
        self.filesystem.write(&uri, &content).await?;

        Ok(uri)
    }

    /// Save a message and trigger layer generation
    ///
    /// This method saves the message and generates L0/L1 layers if llm_client is provided
    pub async fn save_message_with_layers(
        &self,
        thread_id: &str,
        message: &Message,
        layer_manager: &LayerManager,
    ) -> Result<String> {
        // First save the message
        let uri = self.save_message(thread_id, message).await?;

        // Generate L0 and L1 layers for the thread
        // This is done asynchronously and errors are logged but don't fail the save
        let thread_uri = format!("cortex://session/{}", thread_id);
        let content = message.to_markdown();
        if let Err(e) = layer_manager
            .generate_all_layers(&thread_uri, &content)
            .await
        {
            tracing::warn!("Failed to generate layers for thread {}: {}", thread_id, e);
        }

        Ok(uri)
    }

    /// Load a message from URI
    pub async fn load_message(&self, uri: &str) -> Result<Message> {
        let content = self.filesystem.read(uri).await?;

        // Parse markdown to extract message
        // This is a simplified implementation
        // In production, you'd want more robust parsing

        let lines: Vec<&str> = content.lines().collect();

        // Extract ID
        let id = lines
            .iter()
            .find(|l| l.starts_with("**ID**:"))
            .and_then(|l| l.split('`').nth(1))
            .unwrap_or("unknown")
            .to_string();

        // Extract role
        let role = if content.contains("User") {
            MessageRole::User
        } else if content.contains("Assistant") {
            MessageRole::Assistant
        } else {
            MessageRole::System
        };

        // Extract content
        let content_start = content.find("## Content").unwrap_or(0) + 12;
        let content_end = content.find("## Metadata").unwrap_or(content.len());
        let message_content = content[content_start..content_end].trim().to_string();

        // Extract timestamp from filename or content
        let timestamp = Utc::now(); // Simplified: should parse from content

        Ok(Message {
            id,
            role,
            content: message_content,
            timestamp,
            created_at: timestamp,
            metadata: None,
        })
    }

    /// List all messages in a thread
    pub async fn list_messages(&self, thread_id: &str) -> Result<Vec<String>> {
        let timeline_uri = format!("cortex://session/{}/timeline", thread_id);

        // 🔧 Recursively list all .md files in timeline subdirectories
        let mut messages = Vec::new();
        self.collect_message_uris_recursive(&timeline_uri, &mut messages).await?;

        Ok(messages)
    }
    
    /// Recursively collect message URIs from timeline directory
    fn collect_message_uris_recursive<'a>(
        &'a self,
        uri: &'a str,
        result: &'a mut Vec<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            match self.filesystem.list(uri).await {
                Ok(entries) => {
                    for entry in entries {
                        if entry.is_directory && !entry.name.starts_with('.') {
                            // Recursively explore subdirectories
                            self.collect_message_uris_recursive(&entry.uri, result).await?;
                        } else if entry.name.ends_with(".md") && !entry.name.starts_with('.') {
                            // Add message file URI
                            result.push(entry.uri.clone());
                        }
                    }
                }
                Err(e) => {
                    // Directory might not exist yet, that's okay
                    tracing::debug!("Failed to list directory {}: {}", uri, e);
                }
            }
            Ok(())
        })
    }

    /// Delete a message
    pub async fn delete_message(&self, uri: &str) -> Result<()> {
        self.filesystem.delete(uri).await
    }

    /// Batch save messages
    pub async fn batch_save(&self, thread_id: &str, messages: &[Message]) -> Result<Vec<String>> {
        let mut uris = Vec::new();

        for message in messages {
            let uri = self.save_message(thread_id, message).await?;
            uris.push(uri);
        }

        Ok(uris)
    }
}

// 核心功能测试已迁移至 cortex-mem-tools/tests/core_functionality_tests.rs
