//! HTTP adapter over `zeroclaw_runtime::skills::SkillsService`.
//!
//! Thin handlers — every endpoint translates request shape → `SkillsService`
//! call → response shape. No filesystem logic, no validation, no error
//! mapping that isn't already encoded in `SkillsService`. The dashboard,
//! the CLI (`zeroclaw skills add/edit/bundle ...`), and the future TUI all
//! reach the same canonical implementation through their respective surface.

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use zeroclaw_runtime::rpc::types::{
    AgentSkillEntry, AgentSkillsResult, SkillBundleEntry, SkillListEntry, SkillsBundlesResult,
    SkillsListResult, SkillsReadResult,
};
use zeroclaw_runtime::skills::{
    EffectiveSkill, RemoveMode, ScaffoldOptions, ServiceError, SkillFrontmatter, SkillOrigin,
    SkillsService,
};

use super::AppState;
use super::api::require_auth;

// ── HTTP-specific request shapes (not shared) ───────────────────────

#[derive(Debug, Deserialize)]
pub struct SkillCreateBody {
    pub name: String,
    pub frontmatter: SkillFrontmatter,
    /// Initial markdown body. When empty, the service writes a default
    /// `# <Title>` heading derived from the skill name.
    #[serde(default)]
    pub body: String,
    /// Skip scaffolding the optional `scripts/`, `references/`, `assets/`
    /// subdirs. Defaults to `false` (create them).
    #[serde(default)]
    pub no_scaffold: bool,
}

#[derive(Debug, Deserialize)]
pub struct SkillWriteBody {
    pub frontmatter: SkillFrontmatter,
    #[serde(default)]
    pub body: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct DeleteQuery {
    /// When `true`, hard-delete the skill instead of archiving. Defaults to
    /// `false` — same as `RemoveMode::Archive`.
    #[serde(default)]
    pub purge: bool,
}

// ── Handlers ────────────────────────────────────────────────────────

/// `GET /api/skills/bundles`
pub async fn handle_list_bundles(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let config = state.config.read().clone();
    let install_root = config.install_root_dir();
    let service = SkillsService::new(&config, install_root);

    match service.list_bundles() {
        Ok(bundles) => Json(SkillsBundlesResult {
            bundles: bundles
                .into_iter()
                .map(|b| SkillBundleEntry {
                    alias: b.alias,
                    directory: b.directory.display().to_string(),
                    include: b.include,
                    exclude: b.exclude,
                })
                .collect(),
        })
        .into_response(),
        Err(e) => service_error_response(e),
    }
}

/// `GET /api/skills/bundles/:alias/skills`
pub async fn handle_list_skills(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(alias): Path<String>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let config = state.config.read().clone();
    let install_root = config.install_root_dir();
    let service = SkillsService::new(&config, install_root);

    match service.list_skills(Some(&alias)) {
        Ok(skills) => Json(SkillsListResult {
            skills: skills
                .into_iter()
                .map(|s| SkillListEntry {
                    bundle: s.r#ref.bundle().to_string(),
                    name: s.r#ref.name().to_string(),
                    directory: s.directory.display().to_string(),
                    frontmatter: s.frontmatter,
                })
                .collect(),
        })
        .into_response(),
        Err(e) => service_error_response(e),
    }
}

/// `GET /api/agents/:alias/skills` — the agent's *effective* resolved skill set
/// (workspace / open-skills / plugin / bundle), with provenance (#7757). Unlike
/// `/api/skills/bundles/:alias/skills` (bundle-only), this mirrors what the
/// runtime actually loads for the agent, so the dashboard stops rendering an
/// empty page when an agent has workspace/open-skills/plugin skills.
pub async fn handle_agent_skills(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(alias): Path<String>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let config = state.config.read().clone();
    let install_root = config.install_root_dir();
    let service = SkillsService::new(&config, install_root);

    match service.resolve_effective_skills(&alias) {
        Ok(skills) => Json(AgentSkillsResult {
            agent: alias,
            skills: skills.into_iter().map(agent_skill_entry).collect(),
        })
        .into_response(),
        Err(e) => service_error_response(e),
    }
}

/// Map a runtime [`EffectiveSkill`] to its flat wire shape (`origin` string +
/// optional `plugin`/`bundle` detail). `editable`/`directory` pass through.
fn agent_skill_entry(s: EffectiveSkill) -> AgentSkillEntry {
    let (origin, plugin, bundle) = match s.origin {
        SkillOrigin::Workspace => ("workspace", None, None),
        SkillOrigin::OpenSkills => ("open-skills", None, None),
        SkillOrigin::Plugin(p) => ("plugin", Some(p), None),
        SkillOrigin::Bundle(a) => ("bundle", None, Some(a)),
    };
    AgentSkillEntry {
        name: s.name,
        description: s.description,
        origin: origin.to_string(),
        plugin,
        bundle,
        directory: s.directory.map(|d| d.display().to_string()),
        editable: s.editable,
    }
}

/// `POST /api/skills/bundles/:alias/skills`
pub async fn handle_create_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(alias): Path<String>,
    Json(body): Json<SkillCreateBody>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let config = state.config.read().clone();
    let install_root = config.install_root_dir();
    let service = SkillsService::new(&config, install_root);

    let target = match service.resolve_ref(&body.name, Some(&alias)) {
        Ok(r) => r,
        Err(e) => return service_error_response(e),
    };
    match service.scaffold_skill(
        &target,
        body.frontmatter,
        ScaffoldOptions {
            create_optional_subdirs: !body.no_scaffold,
            body: body.body,
        },
    ) {
        Ok(path) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "bundle": target.bundle(),
                "name": target.name(),
                "directory": path.display().to_string(),
            })),
        )
            .into_response(),
        Err(e) => service_error_response(e),
    }
}

/// `GET /api/skills/bundles/:alias/skills/:name`
pub async fn handle_read_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((alias, name)): Path<(String, String)>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let config = state.config.read().clone();
    let install_root = config.install_root_dir();
    let service = SkillsService::new(&config, install_root);

    let target = match service.resolve_ref(&name, Some(&alias)) {
        Ok(r) => r,
        Err(e) => return service_error_response(e),
    };
    match service.read_skill(&target) {
        Ok(doc) => Json(SkillsReadResult {
            bundle: target.bundle().to_string(),
            name: target.name().to_string(),
            frontmatter: doc.frontmatter,
            body: doc.body,
        })
        .into_response(),
        Err(e) => service_error_response(e),
    }
}

/// `PUT /api/skills/bundles/:alias/skills/:name`
pub async fn handle_write_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((alias, name)): Path<(String, String)>,
    Json(body): Json<SkillWriteBody>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let config = state.config.read().clone();
    let install_root = config.install_root_dir();
    let service = SkillsService::new(&config, install_root);

    let target = match service.resolve_ref(&name, Some(&alias)) {
        Ok(r) => r,
        Err(e) => return service_error_response(e),
    };
    let doc = zeroclaw_runtime::skills::SkillDocument {
        frontmatter: body.frontmatter,
        body: body.body,
    };
    match service.write_skill(&target, &doc) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => service_error_response(e),
    }
}

/// `DELETE /api/skills/bundles/:alias/skills/:name?purge=true`
pub async fn handle_delete_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((alias, name)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<DeleteQuery>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let config = state.config.read().clone();
    let install_root = config.install_root_dir();
    let service = SkillsService::new(&config, install_root);

    let target = match service.resolve_ref(&name, Some(&alias)) {
        Ok(r) => r,
        Err(e) => return service_error_response(e),
    };
    let mode = if q.purge {
        RemoveMode::Purge
    } else {
        RemoveMode::Archive
    };
    match service.remove_skill(&target, mode) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => service_error_response(e),
    }
}

// ── Error mapping ───────────────────────────────────────────────────

fn service_error_response(err: ServiceError) -> Response {
    let status = match &err {
        ServiceError::Ref(_) => StatusCode::BAD_REQUEST,
        ServiceError::Bundle(_) => StatusCode::BAD_REQUEST,
        ServiceError::Scaffold(_) => StatusCode::BAD_REQUEST,
        ServiceError::DocumentParse(_) => StatusCode::UNPROCESSABLE_ENTITY,
        ServiceError::NotFound(_) => StatusCode::NOT_FOUND,
        ServiceError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(serde_json::json!({
            "error": format!("{}", err),
        })),
    )
        .into_response()
}
