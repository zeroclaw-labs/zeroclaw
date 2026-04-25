//! End-to-end test for `augusta brain compile`.
//!
//! Spins up a wiremock server impersonating Paperclip, points the compiler at
//! a temp brain dir + temp instances root, and asserts AGENTS.md / SOUL.md /
//! TOOLS.md land under the expected per-agent path with the expected sections.
//!
//! This catches three things the unit tests do not:
//!   1. agents_api JSON wire format (camelCase deserialization).
//!   2. End-to-end glue: HTTP -> render -> atomic write.
//!   3. Path construction for `<root>/companies/<co>/agents/<id>/instructions/`.

use std::fs;
use std::path::Path;

use lightwave_sys::brain::compile::{run, CompileOptions};
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn write(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, body).unwrap();
}

fn write_minimal_brain(brain: &Path) {
    write(
        brain,
        "soul/mind.yaml",
        "thesis: Amplify perspective.\nhow_we_think:\n  - Embrace reality.\n",
    );
    write(
        brain,
        "soul/voice.yaml",
        "communication:\n  - Lead with the headline.\n",
    );
    write(brain, "soul/judgment.yaml", "default_bias: act_first\n");
    write(
        brain,
        "cortex/agents/swarm.yaml",
        r"agents:
  v_scrum:
    role: scrum manager
    domain: agile delivery
    tier: 1
    channels:
      - slack
",
    );
}

async fn mount_paperclip(server: &MockServer, agent_id: &str, company_id: &str) {
    Mock::given(method("GET"))
        .and(path("/api/companies"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": company_id, "status": "active"}
        ])))
        .mount(server)
        .await;

    Mock::given(method("GET"))
        .and(path(format!("/api/companies/{company_id}/agents")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": agent_id,
                "companyId": company_id,
                "name": "Scrum Manager",
                "title": "Scrum manager",
                "capabilities": "Coordinates sprints and unblocks workers.",
                "reportsTo": "gm-001",
            }
        ])))
        .mount(server)
        .await;
}

#[tokio::test]
async fn compile_writes_agent_bundle_under_paperclip_root() {
    let brain_tmp = TempDir::new().unwrap();
    let instances_tmp = TempDir::new().unwrap();
    write_minimal_brain(brain_tmp.path());

    let server = MockServer::start().await;
    let agent_id = "agent-abc";
    let company_id = "co-xyz";
    mount_paperclip(&server, agent_id, company_id).await;

    let report = run(CompileOptions {
        brain_dir: brain_tmp.path().to_path_buf(),
        agent_id: None,
        force: false,
        dry_run: false,
        paperclip_host: server.uri(),
        instances_root: Some(instances_tmp.path().to_path_buf()),
    })
    .await
    .expect("compile run");

    assert_eq!(report.total_agents, 1);
    assert_eq!(report.written, 1);
    assert_eq!(report.skipped_unchanged, 0);
    assert_eq!(report.failed, 0);
    assert!(report.errors.is_empty(), "errors: {:?}", report.errors);

    let agent_dir = instances_tmp
        .path()
        .join("companies")
        .join(company_id)
        .join("agents")
        .join(agent_id)
        .join("instructions");

    let agents_md = fs::read_to_string(agent_dir.join("AGENTS.md")).unwrap();
    let soul_md = fs::read_to_string(agent_dir.join("SOUL.md")).unwrap();
    let tools_md = fs::read_to_string(agent_dir.join("TOOLS.md")).unwrap();

    assert!(agents_md.starts_with("---\ngenerated_by: augusta brain compile\n"));
    assert!(agents_md.contains("# Scrum Manager"));
    assert!(agents_md.contains("Coordinates sprints"));
    assert!(agents_md.contains("agile delivery"));
    assert!(agents_md.contains("Tier:** 1"));
    assert!(agents_md.contains("gm-001"), "reports_to should appear");

    assert!(soul_md.contains("# Soul — Scrum Manager"));
    assert!(soul_md.contains("Amplify perspective"));
    assert!(soul_md.contains("Lead with the headline"));

    assert!(tools_md.contains("# Tools — Scrum Manager"));
    assert!(tools_md.contains("slack"));
    assert!(tools_md.contains("brain.query"));
}

#[tokio::test]
async fn compile_is_idempotent_unchanged_skips_on_second_run() {
    let brain_tmp = TempDir::new().unwrap();
    let instances_tmp = TempDir::new().unwrap();
    write_minimal_brain(brain_tmp.path());

    let server = MockServer::start().await;
    mount_paperclip(&server, "agent-1", "co-1").await;

    let opts = || CompileOptions {
        brain_dir: brain_tmp.path().to_path_buf(),
        agent_id: None,
        force: false,
        dry_run: false,
        paperclip_host: server.uri(),
        instances_root: Some(instances_tmp.path().to_path_buf()),
    };

    let first = run(opts()).await.unwrap();
    assert_eq!(first.written, 1);
    assert_eq!(first.skipped_unchanged, 0);

    let second = run(opts()).await.unwrap();
    assert_eq!(second.written, 0);
    assert_eq!(second.skipped_unchanged, 1);
}

#[tokio::test]
async fn compile_dry_run_writes_nothing() {
    let brain_tmp = TempDir::new().unwrap();
    let instances_tmp = TempDir::new().unwrap();
    write_minimal_brain(brain_tmp.path());

    let server = MockServer::start().await;
    mount_paperclip(&server, "agent-dry", "co-dry").await;

    let report = run(CompileOptions {
        brain_dir: brain_tmp.path().to_path_buf(),
        agent_id: None,
        force: false,
        dry_run: true,
        paperclip_host: server.uri(),
        instances_root: Some(instances_tmp.path().to_path_buf()),
    })
    .await
    .unwrap();

    assert_eq!(report.total_agents, 1);
    assert_eq!(report.written, 1, "WouldWrite counts toward written");
    assert!(report.dry_run);

    let agent_dir = instances_tmp
        .path()
        .join("companies/co-dry/agents/agent-dry/instructions");
    assert!(
        !agent_dir.exists(),
        "dry-run must not create instructions dir"
    );
}

#[tokio::test]
async fn compile_skips_inactive_companies() {
    let brain_tmp = TempDir::new().unwrap();
    let instances_tmp = TempDir::new().unwrap();
    write_minimal_brain(brain_tmp.path());

    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/companies"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": "co-active", "status": "active"},
            {"id": "co-archived", "status": "archived"}
        ])))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/companies/co-active/agents"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": "a1", "companyId": "co-active", "name": "A1"}
        ])))
        .mount(&server)
        .await;

    // /api/companies/co-archived/agents intentionally not mounted — fetching
    // it would 404 and fail the run. Test passes only if archived is skipped.

    let report = run(CompileOptions {
        brain_dir: brain_tmp.path().to_path_buf(),
        agent_id: None,
        force: false,
        dry_run: false,
        paperclip_host: server.uri(),
        instances_root: Some(instances_tmp.path().to_path_buf()),
    })
    .await
    .unwrap();

    assert_eq!(report.total_agents, 1);
    assert_eq!(report.written, 1);
}

#[tokio::test]
async fn compile_agent_id_filter_only_writes_target() {
    let brain_tmp = TempDir::new().unwrap();
    let instances_tmp = TempDir::new().unwrap();
    write_minimal_brain(brain_tmp.path());

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/companies"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": "co-1", "status": "active"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/companies/co-1/agents"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": "a1", "companyId": "co-1", "name": "A1"},
            {"id": "a2", "companyId": "co-1", "name": "A2"}
        ])))
        .mount(&server)
        .await;

    let report = run(CompileOptions {
        brain_dir: brain_tmp.path().to_path_buf(),
        agent_id: Some("a2".into()),
        force: false,
        dry_run: false,
        paperclip_host: server.uri(),
        instances_root: Some(instances_tmp.path().to_path_buf()),
    })
    .await
    .unwrap();

    assert_eq!(report.total_agents, 1);
    assert_eq!(report.written, 1);
    assert!(instances_tmp
        .path()
        .join("companies/co-1/agents/a2/instructions/AGENTS.md")
        .exists());
    assert!(!instances_tmp
        .path()
        .join("companies/co-1/agents/a1/instructions/AGENTS.md")
        .exists());
}
