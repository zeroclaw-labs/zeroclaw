use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zeroclaw_providers::ModelCatalogClient;

// The production crate gates SecurityPolicy behind internal modules; the
// test exercises the tool via its public Tool trait impl.
use zeroclaw_api::tool::Tool;
use zeroclaw_runtime::security::SecurityPolicy;
use zeroclaw_runtime::tools::model_switch::ModelSwitchTool;

#[tokio::test]
async fn list_models_returns_live_catalog() {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "data": [
            {"id": "claude-opus-4-7",   "object": "model", "owned_by": "anthropic"},
            {"id": "claude-sonnet-4-6", "object": "model", "owned_by": "anthropic"}
        ]
    });
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let catalog = Arc::new(
        ModelCatalogClient::new(
            format!("{}/v1", server.uri()),
            "test-key",
            std::path::PathBuf::from("/nonexistent"),
        )
        .unwrap(),
    );

    let tool = ModelSwitchTool::new(Arc::new(SecurityPolicy::default()))
        .with_catalog(catalog);

    let args = serde_json::json!({"action": "list_models"});
    let result = tool.execute(args).await.unwrap();
    assert!(result.success, "expected success, got {:?}", result.error);
    let v: serde_json::Value = serde_json::from_str(&result.output).unwrap();
    let ids: Vec<&str> = v["models"].as_array().unwrap()
        .iter().map(|x| x.as_str().unwrap()).collect();
    assert_eq!(ids, vec!["claude-opus-4-7", "claude-sonnet-4-6"]);
    assert_eq!(v["source"], "live");
}

use std::io::Write;

fn write_tiers(yaml: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(yaml.as_bytes()).unwrap();
    f
}

#[tokio::test]
async fn list_tiers_returns_yaml_contents() {
    let server = MockServer::start().await;
    let f = write_tiers(
        "tiers:\n  - name: chat\n    model: claude-sonnet-4-6\n    description: default\n  - name: thinking\n    model: claude-opus-4-7\n    description: deep reasoning\n",
    );

    let catalog = Arc::new(
        ModelCatalogClient::new(
            format!("{}/v1", server.uri()),
            "test-key",
            f.path().to_path_buf(),
        )
        .unwrap(),
    );
    let tool = ModelSwitchTool::new(Arc::new(SecurityPolicy::default()))
        .with_catalog(catalog);

    let result = tool.execute(serde_json::json!({"action": "list_tiers"})).await.unwrap();
    assert!(result.success, "error: {:?}", result.error);
    let v: serde_json::Value = serde_json::from_str(&result.output).unwrap();
    let names: Vec<&str> = v["tiers"].as_array().unwrap()
        .iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["chat", "thinking"]);
}
