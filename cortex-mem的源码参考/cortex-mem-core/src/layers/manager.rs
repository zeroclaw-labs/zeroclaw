use crate::{ContextLayer, CortexFilesystem, FilesystemOperations, Result, FileEntry};
use crate::llm::LLMClient;
use std::sync::Arc;

use super::generator::{AbstractGenerator, OverviewGenerator};

/// Layer Manager
/// 
/// Manages the three-layer memory architecture (L0/L1/L2)
/// 
/// LLM client is mandatory for high-quality layer generation.
pub struct LayerManager {
    filesystem: Arc<CortexFilesystem>,
    abstract_gen: AbstractGenerator,
    overview_gen: OverviewGenerator,
    llm_client: Arc<dyn LLMClient>,
}

impl LayerManager {
    /// Create a new LayerManager with mandatory LLM client
    pub fn new(filesystem: Arc<CortexFilesystem>, llm_client: Arc<dyn LLMClient>) -> Self {
        Self {
            filesystem,
            abstract_gen: AbstractGenerator::new(),
            overview_gen: OverviewGenerator::new(),
            llm_client,
        }
    }
    
    /// Load content for a specific layer
    pub async fn load(&self, uri: &str, layer: ContextLayer) -> Result<String> {
        match layer {
            ContextLayer::L0Abstract => self.load_abstract(uri).await,
            ContextLayer::L1Overview => self.load_overview(uri).await,
            ContextLayer::L2Detail => self.load_detail(uri).await,
        }
    }
    
    /// Load L0 abstract layer
    ///
    /// IMPORTANT: This method does NOT generate layers on-demand to avoid
    /// blocking the agent's response. Layer generation should be done
    /// asynchronously via MemoryEventCoordinator.
    ///
    /// If the abstract doesn't exist, returns an error instead of generating.
    async fn load_abstract(&self, uri: &str) -> Result<String> {
        let abstract_uri = Self::get_layer_uri(uri, ContextLayer::L0Abstract);
        
        // If exists, read it
        if self.filesystem.exists(&abstract_uri).await? {
            return self.filesystem.read(&abstract_uri).await;
        }
        
        // Check if URI is a directory (doesn't end with .md)
        let is_directory = !uri.ends_with(".md");
        
        if is_directory {
            // For directories, abstract should be pre-generated via layers ensure-all
            // or asynchronously via MemoryEventCoordinator
            return Err(crate::Error::Other(format!(
                "Abstract not found for directory '{}'. Layer generation is asynchronous. \
                 The abstract will be generated in the background and available shortly.",
                uri
            )));
        }
        
        // For files, also don't generate on-demand to avoid blocking
        // Return error indicating the layer is being generated asynchronously
        return Err(crate::Error::Other(format!(
            "Abstract not found for '{}'. Layer generation is asynchronous. \
             Try again later or use 'read' tool for full content.",
            uri
        )));
    }
    
    /// Load L1 overview layer
    ///
    /// IMPORTANT: This method does NOT generate layers on-demand to avoid
    /// blocking the agent's response. Layer generation should be done
    /// asynchronously via MemoryEventCoordinator.
    ///
    /// If the overview doesn't exist, returns an error instead of generating.
    async fn load_overview(&self, uri: &str) -> Result<String> {
        let overview_uri = Self::get_layer_uri(uri, ContextLayer::L1Overview);
        
        if self.filesystem.exists(&overview_uri).await? {
            return self.filesystem.read(&overview_uri).await;
        }
        
        // Don't generate on-demand to avoid blocking the agent
        return Err(crate::Error::Other(format!(
            "Overview not found for '{}'. Layer generation is asynchronous. \
             Try again later or use 'read' tool for full content.",
            uri
        )));
    }
    
    /// Load L2 detail layer (original content)
    async fn load_detail(&self, uri: &str) -> Result<String> {
        self.filesystem.read(uri).await
    }
    
    /// Generate all layers for a new memory (LLM is mandatory)
    pub async fn generate_all_layers(&self, uri: &str, content: &str) -> Result<()> {
        // 1. Write L2 (detail)
        self.filesystem.write(uri, content).await?;
        
        // 2. Generate L0 abstract using LLM
        let abstract_text = self.abstract_gen.generate_with_llm(content, &self.llm_client).await?;
        let abstract_uri = Self::get_layer_uri(uri, ContextLayer::L0Abstract);
        self.filesystem.write(&abstract_uri, &abstract_text).await?;
        
        // 3. Generate L1 overview using LLM
        let overview = self.overview_gen.generate_with_llm(content, &self.llm_client).await?;
        let overview_uri = Self::get_layer_uri(uri, ContextLayer::L1Overview);
        self.filesystem.write(&overview_uri, &overview).await?;
        
        Ok(())
    }
    
    /// Generate L0/L1 layers for a timeline directory or session
    /// 
    /// If `timeline_uri` points to a session root (e.g., cortex://session/{id}/timeline),
    /// aggregates ALL messages across all dates for comprehensive summary.
    /// If it points to a specific date directory, only summarizes that day's messages.
    pub async fn generate_timeline_layers(&self, timeline_uri: &str) -> Result<()> {
        use tracing::{debug, info};
        
        info!("Generating timeline layers for {}", timeline_uri);
        
        // Determine if this is a session-level or date-level timeline
        let is_session_level = !timeline_uri.contains("/timeline/20"); // Session-level if no date path
        
        // 1. Read all messages in timeline (recursively if session-level)
        let entries = if is_session_level {
            self.collect_all_timeline_messages(timeline_uri).await?
        } else {
            self.filesystem.list(timeline_uri).await?
        };
        
        let mut messages = Vec::new();
        
        for entry in entries {
            if entry.name.starts_with('.') {
                continue;
            }
            
            if entry.name.ends_with(".md") && !entry.is_directory {
                match self.filesystem.read(&entry.uri).await {
                    Ok(content) => messages.push((entry.uri.clone(), content)),
                    Err(e) => debug!("Failed to read {}: {}", entry.uri, e),
                }
            }
        }
        
        if messages.is_empty() {
            debug!("No messages found in {}", timeline_uri);
            return Ok(());
        }
        
        // 2. Aggregate all content
        let mut all_content = String::new();
        all_content.push_str(&format!("# Timeline: {}\n\n", timeline_uri));
        all_content.push_str(&format!("Total messages: {}\n\n", messages.len()));
        
        for (idx, (_uri, content)) in messages.iter().enumerate() {
            all_content.push_str(&format!("## Message {}\n\n", idx + 1));
            all_content.push_str(content);
            all_content.push_str("\n\n---\n\n");
        }
        
        // 3. Generate L0 abstract using LLM
        let abstract_text = self.abstract_gen.generate_with_llm(&all_content, &self.llm_client).await?;
        let abstract_uri = format!("{}/.abstract.md", timeline_uri);
        self.filesystem.write(&abstract_uri, &abstract_text).await?;
        info!("Generated L0 abstract: {}", abstract_uri);
        
        // 4. Generate L1 overview using LLM
        let overview = self.overview_gen.generate_with_llm(&all_content, &self.llm_client).await?;
        let overview_uri = format!("{}/.overview.md", timeline_uri);
        self.filesystem.write(&overview_uri, &overview).await?;
        info!("Generated L1 overview: {}", overview_uri);
        
        Ok(())
    }
    
    /// Recursively collect all message entries from timeline subdirectories
    async fn collect_all_timeline_messages(&self, timeline_root: &str) -> Result<Vec<FileEntry>> {
        let mut all_entries = Vec::new();
        self.collect_messages_recursive(timeline_root, &mut all_entries).await?;
        Ok(all_entries)
    }
    
    fn collect_messages_recursive<'a>(
        &'a self,
        uri: &'a str,
        entries: &'a mut Vec<FileEntry>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let dir_entries = self.filesystem.list(uri).await?;
            
            for entry in dir_entries {
                if entry.name.starts_with('.') {
                    continue;
                }
                
                if entry.is_directory {
                    // Recurse into subdirectories
                    self.collect_messages_recursive(&entry.uri, entries).await?;
                } else if entry.name.ends_with(".md") {
                    entries.push(entry);
                }
            }
            
            Ok(())
        })
    }
    
    /// Get layer URI for a base URI
    /// 
    /// For file URIs (ending with .md): extract directory and append layer file
    /// For directory URIs: directly append layer file
    fn get_layer_uri(base_uri: &str, layer: ContextLayer) -> String {
        match layer {
            ContextLayer::L0Abstract => {
                // Check if URI points to a file (ends with .md) or a directory
                let dir = if base_uri.ends_with(".md") {
                    // File URI: extract directory path
                    base_uri.rsplit_once('/').map(|(dir, _)| dir).unwrap_or(base_uri)
                } else {
                    // Directory URI: use as-is
                    base_uri
                };
                format!("{}/.abstract.md", dir)
            }
            ContextLayer::L1Overview => {
                // Check if URI points to a file (ends with .md) or a directory
                let dir = if base_uri.ends_with(".md") {
                    // File URI: extract directory path
                    base_uri.rsplit_once('/').map(|(dir, _)| dir).unwrap_or(base_uri)
                } else {
                    // Directory URI: use as-is
                    base_uri
                };
                format!("{}/.overview.md", dir)
            }
            ContextLayer::L2Detail => base_uri.to_string(),
        }
    }
}