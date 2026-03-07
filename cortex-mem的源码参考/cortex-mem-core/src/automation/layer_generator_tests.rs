use super::*;
use crate::{CortexFilesystem, FilesystemOperations, llm::{LLMClient, LLMConfig, MemoryExtractionResponse}, Result};
use std::sync::Arc;
use async_trait::async_trait;

/// Mock LLM Client for testing
struct MockLLMClient {
    abstract_response: String,
    overview_response: String,
}

impl MockLLMClient {
    fn new() -> Self {
        Self {
            abstract_response: "Mock abstract summary for testing.".to_string(),
            overview_response: "# Mock Overview\n\nThis is a mock overview for testing purposes.\n\n## Topics\n- Testing\n- Mocking".to_string(),
        }
    }
}

#[async_trait]
impl LLMClient for MockLLMClient {
    async fn complete(&self, _prompt: &str) -> Result<String> {
        Ok(self.abstract_response.clone())
    }

    async fn complete_with_system(&self, system: &str, _prompt: &str) -> Result<String> {
        if system.contains("abstract") || system.contains("摘要") {
            Ok(self.abstract_response.clone())
        } else {
            Ok(self.overview_response.clone())
        }
    }
    
    async fn extract_memories(&self, _prompt: &str) -> Result<MemoryExtractionResponse> {
        Ok(MemoryExtractionResponse {
            facts: vec![],
            decisions: vec![],
            entities: vec![],
        })
    }
    
    async fn extract_structured_facts(&self, _prompt: &str) -> Result<crate::llm::extractor_types::StructuredFactExtraction> {
        Ok(crate::llm::extractor_types::StructuredFactExtraction {
            facts: vec![],
        })
    }
    
    async fn extract_detailed_facts(&self, _prompt: &str) -> Result<crate::llm::extractor_types::DetailedFactExtraction> {
        Ok(crate::llm::extractor_types::DetailedFactExtraction {
            facts: vec![],
        })
    }
    
    fn model_name(&self) -> &str {
        "mock-llm"
    }
    
    fn config(&self) -> &LLMConfig {
        // Return a static config
        static CONFIG: LLMConfig = LLMConfig {
            api_base_url: String::new(),
            api_key: String::new(),
            model_efficient: String::new(),
            temperature: 0.7,
            max_tokens: 2048,
        };
        &CONFIG
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn setup_test_filesystem() -> (Arc<CortexFilesystem>, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let fs = Arc::new(CortexFilesystem::with_tenant(
            temp_dir.path(),
            "test-tenant",
        ));
        fs.initialize().await.unwrap();
        (fs, temp_dir)
    }

    fn mock_llm_client() -> Arc<dyn LLMClient> {
        Arc::new(MockLLMClient::new())
    }

    fn default_config() -> LayerGenerationConfig {
        LayerGenerationConfig {
            batch_size: 2,
            delay_ms: 100,
            auto_generate_on_startup: false,
            abstract_config: AbstractConfig {
                max_tokens: 400,
                max_chars: 2000,
                target_sentences: 2,
            },
            overview_config: OverviewConfig {
                max_tokens: 1500,
                max_chars: 6000,
            },
        }
    }

    #[tokio::test]
    async fn test_scan_all_directories_empty() {
        let (fs, _temp) = setup_test_filesystem().await;
        let generator = LayerGenerator::new(fs, mock_llm_client(), default_config());

        let dirs = generator.scan_all_directories().await.unwrap();

        assert_eq!(dirs.len(), 0, "Empty filesystem should return no directories");
    }

    #[tokio::test]
    async fn test_scan_all_directories_with_files() {
        let (fs, _temp) = setup_test_filesystem().await;

        // Create test directories with files
        fs.write("cortex://user/test-user/preferences/pref1.md", "content").await.unwrap();
        fs.write("cortex://agent/test-agent/cases/case1.md", "content").await.unwrap();
        fs.write("cortex://session/test-session/timeline/2026-02/25/msg1.md", "content").await.unwrap();

        let generator = LayerGenerator::new(fs, mock_llm_client(), default_config());
        let dirs = generator.scan_all_directories().await.unwrap();

        // Should find user/test-user, user/test-user/preferences, agent/test-agent, etc.
        assert!(dirs.len() > 0, "Should find directories");
        assert!(dirs.iter().any(|d| d.contains("preferences")), "Should find preferences dir");
        assert!(dirs.iter().any(|d| d.contains("cases")), "Should find cases dir");
    }

    #[tokio::test]
    async fn test_scan_nested_directories() {
        let (fs, _temp) = setup_test_filesystem().await;

        fs.write("cortex://user/u1/prefs/lang/rust.md", "content").await.unwrap();
        fs.write("cortex://user/u1/prefs/lang/python.md", "content").await.unwrap();

        let generator = LayerGenerator::new(fs, mock_llm_client(), default_config());
        let dirs = generator.scan_all_directories().await.unwrap();

        // Should include all nested levels
        assert!(dirs.iter().any(|d| d.contains("user/u1")));
        assert!(dirs.iter().any(|d| d.contains("prefs")));
        assert!(dirs.iter().any(|d| d.contains("lang")));
    }

    #[tokio::test]
    async fn test_has_layers_both_present() {
        let (fs, _temp) = setup_test_filesystem().await;

        fs.write("cortex://user/test/.abstract.md", "abstract").await.unwrap();
        fs.write("cortex://user/test/.overview.md", "overview").await.unwrap();

        let generator = LayerGenerator::new(fs, mock_llm_client(), default_config());
        let has_layers = generator.has_layers("cortex://user/test").await.unwrap();

        assert!(has_layers, "Should have layers when both files exist");
    }

    #[tokio::test]
    async fn test_has_layers_missing_abstract() {
        let (fs, _temp) = setup_test_filesystem().await;

        fs.write("cortex://user/test/.overview.md", "overview").await.unwrap();

        let generator = LayerGenerator::new(fs, mock_llm_client(), default_config());
        let has_layers = generator.has_layers("cortex://user/test").await.unwrap();

        assert!(!has_layers, "Should not have layers when abstract is missing");
    }

    #[tokio::test]
    async fn test_has_layers_missing_overview() {
        let (fs, _temp) = setup_test_filesystem().await;

        fs.write("cortex://user/test/.abstract.md", "abstract").await.unwrap();

        let generator = LayerGenerator::new(fs, mock_llm_client(), default_config());
        let has_layers = generator.has_layers("cortex://user/test").await.unwrap();

        assert!(!has_layers, "Should not have layers when overview is missing");
    }

    #[tokio::test]
    async fn test_has_layers_both_missing() {
        let (fs, _temp) = setup_test_filesystem().await;

        fs.write("cortex://user/test/file.md", "content").await.unwrap();

        let generator = LayerGenerator::new(fs, mock_llm_client(), default_config());
        let has_layers = generator.has_layers("cortex://user/test").await.unwrap();

        assert!(!has_layers, "Should not have layers when both files are missing");
    }

    #[tokio::test]
    async fn test_filter_missing_layers() {
        let (fs, _temp) = setup_test_filesystem().await;

        // Create one complete directory
        fs.write("cortex://user/complete/.abstract.md", "a").await.unwrap();
        fs.write("cortex://user/complete/.overview.md", "o").await.unwrap();
        fs.write("cortex://user/complete/file.md", "content").await.unwrap();

        // Create two incomplete directories
        fs.write("cortex://user/missing1/file.md", "content").await.unwrap();
        fs.write("cortex://user/missing2/file.md", "content").await.unwrap();

        let generator = LayerGenerator::new(fs, mock_llm_client(), default_config());

        let all_dirs = vec![
            "cortex://user/complete".to_string(),
            "cortex://user/missing1".to_string(),
            "cortex://user/missing2".to_string(),
        ];

        let missing = generator.filter_missing_layers(&all_dirs).await.unwrap();

        assert_eq!(missing.len(), 2, "Should find 2 missing directories");
        assert!(missing.contains(&"cortex://user/missing1".to_string()));
        assert!(missing.contains(&"cortex://user/missing2".to_string()));
        assert!(!missing.contains(&"cortex://user/complete".to_string()));
    }

    #[tokio::test]
    async fn test_ensure_all_layers_empty_filesystem() {
        let (fs, _temp) = setup_test_filesystem().await;
        let generator = LayerGenerator::new(fs, mock_llm_client(), default_config());

        let stats = generator.ensure_all_layers().await.unwrap();

        assert_eq!(stats.total, 0);
        assert_eq!(stats.generated, 0);
        assert_eq!(stats.failed, 0);
    }

    #[tokio::test]
    async fn test_ensure_all_layers_with_missing() {
        let (fs, _temp) = setup_test_filesystem().await;

        // Create directories with content but no L0/L1
        fs.write("cortex://user/test1/pref.md", "User preference content for testing").await.unwrap();
        fs.write("cortex://user/test2/pref.md", "Another preference for testing").await.unwrap();

        let generator = LayerGenerator::new(fs, mock_llm_client(), default_config());
        let stats = generator.ensure_all_layers().await.unwrap();

        // Should attempt to generate for missing directories
        assert!(stats.total > 0, "Should find directories needing generation");
        assert!(stats.generated > 0 || stats.failed > 0, "Should attempt generation");
    }
    
    #[tokio::test]
    async fn test_regenerate_oversized_abstracts_no_oversized() {
        let (fs, _temp) = setup_test_filesystem().await;

        // Create a normal-sized abstract
        let normal_content = "Short abstract.\n\n**Added**: 2026-02-25 12:00:00 UTC";
        fs.write("cortex://user/test/.abstract.md", normal_content).await.unwrap();
        fs.write("cortex://user/test/file.md", "content").await.unwrap();

        let generator = LayerGenerator::new(fs.clone(), mock_llm_client(), default_config());
        let stats = generator.regenerate_oversized_abstracts().await.unwrap();

        assert_eq!(stats.total, 0, "Should not find any oversized abstracts");
        assert_eq!(stats.regenerated, 0);
    }
}
