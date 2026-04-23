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

#[tokio::test]
async fn list_models_uses_cache_on_second_call_within_ttl() {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "data": [{"id": "claude-opus-4-7", "object": "model", "owned_by": "anthropic"}]
    });
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .expect(1) // second call must NOT hit the server
        .mount(&server)
        .await;

    let base = format!("{}/v1", server.uri());
    let client = ModelCatalogClient::new(base, "test-key", tiers_yaml_path()).unwrap();

    let first = client.list_models().await.unwrap();
    let second = client.list_models().await.unwrap();

    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    // wiremock's `.expect(1)` fails the test on drop if the mock was hit more than once.
}

#[tokio::test]
async fn list_models_surfaces_non_2xx_as_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(502).set_body_string("upstream down"))
        .mount(&server)
        .await;

    let base = format!("{}/v1", server.uri());
    let client = ModelCatalogClient::new(base, "test-key", tiers_yaml_path()).unwrap();
    let err = client.list_models().await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("502"), "error should mention 502: {msg}");
    assert!(
        msg.contains("upstream down"),
        "error should include body: {msg}"
    );
}

use std::io::Write;

fn write_tiers_file(yaml: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(yaml.as_bytes()).unwrap();
    f
}

#[tokio::test]
async fn list_tiers_reads_yaml() {
    let f = write_tiers_file(
        "tiers:\n  - name: chat\n    model: claude-sonnet-4-6\n    description: default\n  - name: thinking\n    model: claude-opus-4-7\n    description: deep reasoning\n",
    );
    let server = MockServer::start().await;
    let client = ModelCatalogClient::new(
        format!("{}/v1", server.uri()),
        "test-key",
        f.path().to_path_buf(),
    )
    .unwrap();

    let tiers = client.list_tiers().await.unwrap();
    assert_eq!(tiers.len(), 2);
    assert_eq!(tiers[0].name, "chat");
    assert_eq!(tiers[1].model, "claude-opus-4-7");
}

#[tokio::test]
async fn resolve_tier_returns_model_case_insensitive() {
    let f = write_tiers_file(
        "tiers:\n  - name: thinking\n    model: claude-opus-4-7\n    description: \"\"\n",
    );
    let server = MockServer::start().await;
    let client = ModelCatalogClient::new(
        format!("{}/v1", server.uri()),
        "test-key",
        f.path().to_path_buf(),
    )
    .unwrap();

    let model = client.resolve_tier("Thinking").await.unwrap();
    assert_eq!(model, "claude-opus-4-7");
}

#[tokio::test]
async fn resolve_tier_rejects_unknown_tier() {
    let f = write_tiers_file(
        "tiers:\n  - name: chat\n    model: claude-sonnet-4-6\n    description: \"\"\n",
    );
    let server = MockServer::start().await;
    let client = ModelCatalogClient::new(
        format!("{}/v1", server.uri()),
        "test-key",
        f.path().to_path_buf(),
    )
    .unwrap();

    let err = client.resolve_tier("ultra").await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("unknown tier"));
    assert!(msg.contains("chat"));
}
