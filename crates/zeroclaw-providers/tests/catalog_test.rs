use std::path::PathBuf;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zeroclaw_providers::ModelCatalogClient;

fn tiers_yaml_path() -> PathBuf {
    // tests don't exercise tier resolution in this file; point at a path that
    // does not exist so any accidental read surfaces as an error.
    PathBuf::from("/nonexistent/tiers.yaml")
}

#[tokio::test]
async fn list_models_returns_catalog_from_endpoint() {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "data": [
            {"id": "claude-opus-4-7",   "object": "model", "owned_by": "anthropic"},
            {"id": "claude-sonnet-4-6", "object": "model", "owned_by": "anthropic"}
        ]
    });
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("Authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .expect(1)
        .mount(&server)
        .await;

    let base = format!("{}/v1", server.uri());
    let client = ModelCatalogClient::new(base, "test-key", tiers_yaml_path()).unwrap();
    let models = client.list_models().await.expect("list_models");

    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["claude-opus-4-7", "claude-sonnet-4-6"]);
}
