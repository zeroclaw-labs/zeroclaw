use serde_json::json;
use std::sync::Arc;
use tempfile::TempDir;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};
use zeroclaw_api::tool::Tool;
use zeroclaw_config::autonomy::AutonomyLevel;
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::{FileUploadBundleConfig, FileUploadConfig};
use zeroclaw_tools::file_edit::FileEditTool;
use zeroclaw_tools::file_upload::FileUploadTool;
use zeroclaw_tools::file_upload_bundle::FileUploadBundleTool;
use zeroclaw_tools::file_write::FileWriteTool;
use zeroclaw_tools::wrappers::{PathGuardedTool, RateLimitedTool};

fn policy(workspace: &TempDir) -> SecurityPolicy {
    SecurityPolicy {
        autonomy: AutonomyLevel::Full,
        workspace_dir: workspace.path().to_path_buf(),
        max_actions_per_hour: 100,
        forbidden_paths: vec![],
        ..SecurityPolicy::default()
    }
}

#[tokio::test]
async fn upload_tools_require_read_access_before_sending() {
    for workspace_only in [true, false] {
        for grant in ["write", "read", "read_write", "read_and_write"] {
            let workspace = TempDir::new().unwrap();
            let sibling = TempDir::new().unwrap();
            let target = sibling.path().join("report.txt");
            std::fs::write(&target, "synthetic sibling contents").unwrap();
            std::fs::write(workspace.path().join("own.txt"), "own contents").unwrap();
            let mut security = policy(&workspace);
            security.workspace_only = workspace_only;
            match grant {
                "write" => security
                    .allowed_roots_write_only
                    .push(sibling.path().into()),
                "read" => security.allowed_roots_read_only.push(sibling.path().into()),
                "read_write" => security.allowed_roots.push(sibling.path().into()),
                "read_and_write" => {
                    security.allowed_roots_read_only.push(sibling.path().into());
                    security
                        .allowed_roots_write_only
                        .push(sibling.path().into());
                }
                _ => unreachable!(),
            }
            let security = Arc::new(security);
            let server = MockServer::start().await;
            let allowed = grant != "write";
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(200).set_body_string("accepted"))
                .expect(if allowed { 2 } else { 0 })
                .mount(&server)
                .await;
            let single = FileUploadTool::new(
                security.clone(),
                FileUploadConfig {
                    url: Some(server.uri()),
                    ..FileUploadConfig::default()
                },
            );
            let bundle = FileUploadBundleTool::new(
                security,
                FileUploadBundleConfig {
                    url: Some(server.uri()),
                    ..FileUploadBundleConfig::default()
                },
            );
            // Put a permitted entry first: a later refusal must send no partial bundle.
            let results = [
                single.execute(json!({"file_path": target})).await.unwrap(),
                bundle
                    .execute(json!({"file_paths": ["own.txt", target]}))
                    .await
                    .unwrap(),
            ];
            for result in results {
                assert_eq!(
                    result.success, allowed,
                    "{grant}, workspace_only={workspace_only}: {result:?}"
                );
                if !allowed {
                    assert!(
                        result
                            .error
                            .unwrap()
                            .contains("Resolved path escapes workspace allowlist")
                    );
                    assert!(!result.output.contains("synthetic sibling contents"));
                }
            }
            let requests = server.received_requests().await.unwrap();
            assert_eq!(requests.len(), if allowed { 2 } else { 0 });
            for request in requests {
                assert!(
                    String::from_utf8_lossy(&request.body).contains("synthetic sibling contents")
                );
            }
        }
    }
}

#[tokio::test]
async fn edit_write_only_refuses_before_content_diagnostics_but_write_still_works() {
    for workspace_only in [true, false] {
        let workspace = TempDir::new().unwrap();
        let sibling = TempDir::new().unwrap();
        let target = sibling.path().join("notes.txt");
        let original = "synthetic repeated repeated\n    indented marker\n";
        std::fs::write(&target, original).unwrap();
        let mut security = policy(&workspace);
        security.workspace_only = workspace_only;
        security
            .allowed_roots_write_only
            .push(sibling.path().into());
        let security = Arc::new(security);
        let edit = RateLimitedTool::new(
            PathGuardedTool::new(FileEditTool::new(security.clone()), security.clone()),
            security.clone(),
        );
        let expected = security.resolved_path_violation_message(&target.canonicalize().unwrap());
        for old in ["synthetic", "absent", "repeated", "  indented marker"] {
            let result = edit
                .execute(json!({"path": target, "old_string": old, "new_string": "replacement"}))
                .await
                .unwrap();
            assert!(!result.success, "{result:?}");
            assert_eq!(result.error.as_deref(), Some(expected.as_str()));
            assert_eq!(std::fs::read_to_string(&target).unwrap(), original);
        }
        let write = RateLimitedTool::new(
            PathGuardedTool::new(FileWriteTool::new(security.clone()), security.clone()),
            security,
        );
        let result = write
            .execute(json!({"path": target, "content": "new deposit"}))
            .await
            .unwrap();
        assert!(result.success, "{result:?}");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new deposit");
    }
}

#[tokio::test]
async fn edit_requires_both_tiers_and_checks_the_target_read_grant() {
    for grant in ["read", "read_write", "target_read_and_parent_write"] {
        let workspace = TempDir::new().unwrap();
        let sibling = TempDir::new().unwrap();
        let target = sibling.path().join("notes.txt");
        std::fs::write(&target, "before").unwrap();
        let mut security = policy(&workspace);
        match grant {
            "read" => security.allowed_roots_read_only.push(sibling.path().into()),
            "read_write" => security.allowed_roots.push(sibling.path().into()),
            "target_read_and_parent_write" => {
                security.allowed_roots_read_only.push(target.clone());
                security
                    .allowed_roots_write_only
                    .push(sibling.path().into());
            }
            _ => unreachable!(),
        }
        let security = Arc::new(security);
        let edit = RateLimitedTool::new(
            PathGuardedTool::new(FileEditTool::new(security.clone()), security.clone()),
            security,
        );
        let result = edit
            .execute(json!({"path": target, "old_string": "before", "new_string": "after"}))
            .await
            .unwrap();
        let allowed = grant != "read";
        assert_eq!(result.success, allowed, "{grant}: {result:?}");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            if allowed { "after" } else { "before" }
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn workspace_symlink_does_not_grant_read_access_to_a_write_only_target() {
    let workspace = TempDir::new().unwrap();
    let sibling = TempDir::new().unwrap();
    let target = sibling.path().join("report.txt");
    std::fs::write(&target, "synthetic sibling contents").unwrap();
    std::os::unix::fs::symlink(&target, workspace.path().join("alias.txt")).unwrap();
    let mut security = policy(&workspace);
    security
        .allowed_roots_write_only
        .push(sibling.path().into());
    let security = Arc::new(security);
    let server = MockServer::start().await;
    let single = FileUploadTool::new(
        security.clone(),
        FileUploadConfig {
            url: Some(server.uri()),
            ..FileUploadConfig::default()
        },
    );
    let bundle = FileUploadBundleTool::new(
        security.clone(),
        FileUploadBundleConfig {
            url: Some(server.uri()),
            ..FileUploadBundleConfig::default()
        },
    );
    for result in [
        single
            .execute(json!({"file_path": "alias.txt"}))
            .await
            .unwrap(),
        bundle
            .execute(json!({"file_paths": ["alias.txt"]}))
            .await
            .unwrap(),
    ] {
        assert!(!result.success, "{result:?}");
        assert!(
            result
                .error
                .unwrap()
                .contains("Resolved path escapes workspace allowlist")
        );
    }
    assert!(server.received_requests().await.unwrap().is_empty());
    let edit = FileEditTool::new(security);
    let result = edit
        .execute(
            json!({"path": "alias.txt", "old_string": "synthetic", "new_string": "replacement"}),
        )
        .await
        .unwrap();
    assert!(!result.success);
    assert!(result.error.unwrap().contains("symlink"));
    assert_eq!(
        std::fs::read_to_string(target).unwrap(),
        "synthetic sibling contents"
    );
}
