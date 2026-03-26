# ZeroClaw Skills REST API 实现指南

## 概述

ZeroClaw 目前的 Skills 管理仅通过 CLI (`zeroclaw skills list/install/remove/audit`) 实现，Gateway HTTP API 中没有 `/api/skills` 端点。本指南详细说明如何在后端（Rust）和前端（React Dashboard）两侧增加完整的 Skills API 支持。

---

## 架构总览

```
┌──────────────────┐     REST API      ┌──────────────────────────┐
│  Web Dashboard   │ ◄──────────────► │   Gateway (Axum Router)  │
│  (React + Vite)  │                   │                          │
│                  │   GET /api/skills │  handle_api_skills_list  │
│  Skills.tsx      │   POST /api/skills│  handle_api_skills_inst  │
│                  │   DELETE /api/    │  handle_api_skills_del   │
│                  │   POST /api/      │  handle_api_skills_audit │
└──────────────────┘   skills/:name    └─────────┬────────────────┘
                                                  │
                                        ┌─────────▼────────────────┐
                                        │   src/skills/mod.rs      │
                                        │                          │
                                        │ load_skills_with_config()│
                                        │ install_skill()          │
                                        │ handle_command()         │
                                        │ audit_skill_directory()  │
                                        └──────────────────────────┘
                                                  │
                                        ┌─────────▼────────────────┐
                                        │  ~/.zeroclaw/workspace/  │
                                        │    skills/               │
                                        │      my-skill/           │
                                        │        SKILL.toml        │
                                        │      another/            │
                                        │        SKILL.md          │
                                        └──────────────────────────┘
```

---

## 第一部分：后端 — Rust Gateway API

### 1.1 需要修改的文件

| 文件 | 作用 |
|------|------|
| `src/gateway/api.rs` | 添加 4 个新的 handler 函数 |
| `src/gateway/mod.rs` | 在 Axum Router 中注册新路由 |
| `src/skills/mod.rs` | 提取/公开可复用的函数（目前部分逻辑在 `handle_command` 内部） |

### 1.2 新增 API 端点设计

#### `GET /api/skills` — 列出已安装的 skills

```
GET /api/skills
Authorization: Bearer <token>

Response 200:
{
  "skills": [
    {
      "name": "web-search",
      "description": "Search the web using DuckDuckGo",
      "version": "0.2.1",
      "author": "community",
      "tools": [
        {
          "name": "web_search",
          "description": "Search the web",
          "kind": "shell",
          "args": { "query": "string" }
        }
      ],
      "prompts_count": 2,
      "location": "~/.zeroclaw/workspace/skills/web-search",
      "always": false
    }
  ],
  "open_skills_enabled": true,
  "total": 5
}
```

#### `POST /api/skills/install` — 安装新 skill

```
POST /api/skills/install
Authorization: Bearer <token>
Content-Type: application/json

Request:
{
  "source": "https://github.com/user/my-skill.git"
}
// 或本地路径:
{
  "source": "/path/to/local/skill"
}

Response 200:
{
  "status": "ok",
  "name": "my-skill",
  "audit": {
    "files_scanned": 3,
    "findings": [],
    "is_clean": true
  }
}

Response 400:
{
  "error": "Audit failed",
  "audit": {
    "files_scanned": 3,
    "findings": ["script file detected: setup.sh"],
    "is_clean": false
  }
}
```

#### `DELETE /api/skills/:name` — 删除已安装 skill

```
DELETE /api/skills/web-search
Authorization: Bearer <token>

Response 200:
{ "status": "ok", "deleted": true }

Response 404:
{ "error": "Skill not found", "deleted": false }
```

#### `POST /api/skills/audit` — 审计指定 skill

```
POST /api/skills/audit
Authorization: Bearer <token>
Content-Type: application/json

Request:
{
  "name": "web-search"
}
// 或审计外部源:
{
  "source": "https://github.com/user/my-skill.git"
}

Response 200:
{
  "name": "web-search",
  "files_scanned": 4,
  "findings": [],
  "is_clean": true,
  "summary": "Scanned 4 files, no issues found"
}
```

### 1.3 Rust 实现代码

#### 步骤 1：在 `src/skills/mod.rs` 中公开核心函数

ZeroClaw 的 skills 逻辑目前耦合在 CLI 的 `handle_command()` 函数中（约在 mod.rs 第 635-950 行）。需要将核心逻辑提取为独立的公开函数：

```rust
// src/skills/mod.rs — 新增公开函数

use serde::Serialize;

/// 用于 API 返回的 Skill 摘要信息
#[derive(Serialize, Clone)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub version: String,
    pub author: Option<String>,
    pub tools: Vec<SkillToolInfo>,
    pub prompts_count: usize,
    pub location: Option<String>,
    pub always: bool,
}

#[derive(Serialize, Clone)]
pub struct SkillToolInfo {
    pub name: String,
    pub description: String,
    pub kind: String,
}

impl From<&Skill> for SkillInfo {
    fn from(skill: &Skill) -> Self {
        SkillInfo {
            name: skill.name.clone(),
            description: skill.description.clone(),
            version: skill.version.clone(),
            author: skill.author.clone(),
            tools: skill.tools.iter().map(|t| SkillToolInfo {
                name: t.name.clone(),
                description: t.description.clone(),
                kind: t.kind.clone(),
            }).collect(),
            prompts_count: skill.prompts.len(),
            location: skill.location.as_ref().map(|p| p.display().to_string()),
            always: skill.always,
        }
    }
}

/// 列出所有已加载的 skills — 供 API 和 CLI 共用
pub fn list_skills(config: &Config) -> anyhow::Result<Vec<SkillInfo>> {
    let skills = load_skills_with_config(config)?;
    Ok(skills.iter().map(SkillInfo::from).collect())
}

/// 安装 skill（从 Git 或本地路径）— 提取自 handle_command
pub async fn install_skill_from_source(
    source: &str,
    config: &Config,
) -> anyhow::Result<(String, audit::SkillAuditReport)> {
    // 复用现有逻辑：
    // 1. is_git_source(source) 判断来源类型
    // 2. Git: clone 到临时目录 → audit → 复制到 workspace/skills/
    // 3. Local: canonicalize → audit → 复制到 workspace/skills/
    // 4. 返回 (skill_name, audit_report)
    
    let workspace = config.workspace_dir();
    let skills_dir = workspace.join("skills");
    
    if is_git_source(source) {
        install_git_skill_source(source, &skills_dir).await
    } else {
        let path = std::path::PathBuf::from(source);
        install_local_skill_source(&path, &skills_dir)
    }
}

/// 删除指定 skill — 提取自 handle_command
pub fn remove_skill(name: &str, config: &Config) -> anyhow::Result<bool> {
    // 安全校验 (路径穿越防护)
    if name.contains("..") || name.contains('/') || name.contains('\\') {
        anyhow::bail!("Invalid skill name");
    }
    
    let skills_dir = config.workspace_dir().join("skills");
    let skill_path = skills_dir.join(name);
    let canonical = skill_path.canonicalize()?;
    
    // 确认规范路径在 skills 目录内
    if !canonical.starts_with(skills_dir.canonicalize()?) {
        anyhow::bail!("Path traversal detected");
    }
    
    if canonical.exists() {
        std::fs::remove_dir_all(&canonical)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// 审计指定 skill
pub fn audit_skill_by_name(
    name: &str,
    config: &Config,
) -> anyhow::Result<audit::SkillAuditReport> {
    let skill_path = config.workspace_dir().join("skills").join(name);
    if !skill_path.exists() {
        anyhow::bail!("Skill '{}' not found", name);
    }
    Ok(audit::audit_skill_directory(&skill_path))
}
```

#### 步骤 2：在 `src/gateway/api.rs` 中添加 Handler

```rust
// src/gateway/api.rs — 新增 skills 相关 handler

use crate::skills::{self, SkillInfo};

/// GET /api/skills
pub async fn handle_api_skills_list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e;
    }
    
    let config = state.config.lock().unwrap();
    match skills::list_skills(&config) {
        Ok(skills_list) => {
            let open_skills_enabled = config.skills
                .as_ref()
                .map(|s| s.open_skills_enabled)
                .unwrap_or(false);
            
            Json(serde_json::json!({
                "skills": skills_list,
                "open_skills_enabled": open_skills_enabled,
                "total": skills_list.len()
            })).into_response()
        }
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR,
             Json(serde_json::json!({"error": e.to_string()})))
                .into_response()
        }
    }
}

/// POST /api/skills/install
pub async fn handle_api_skills_install(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e;
    }
    
    let source = match body.get("source").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => {
            return (StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "missing 'source' field"})))
                .into_response();
        }
    };
    
    let config = state.config.lock().unwrap().clone();
    match skills::install_skill_from_source(&source, &config).await {
        Ok((name, report)) => {
            if report.is_clean() {
                Json(serde_json::json!({
                    "status": "ok",
                    "name": name,
                    "audit": {
                        "files_scanned": report.files_scanned,
                        "findings": report.findings,
                        "is_clean": true
                    }
                })).into_response()
            } else {
                (StatusCode::BAD_REQUEST,
                 Json(serde_json::json!({
                    "error": "Audit failed",
                    "audit": {
                        "files_scanned": report.files_scanned,
                        "findings": report.findings,
                        "is_clean": false
                    }
                }))).into_response()
            }
        }
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR,
             Json(serde_json::json!({"error": e.to_string()})))
                .into_response()
        }
    }
}

/// DELETE /api/skills/:name
pub async fn handle_api_skills_remove(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e;
    }
    
    let config = state.config.lock().unwrap();
    match skills::remove_skill(&name, &config) {
        Ok(true) => Json(serde_json::json!({
            "status": "ok", "deleted": true
        })).into_response(),
        Ok(false) => (StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "Skill not found", "deleted": false
            }))).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": e.to_string()
            }))).into_response(),
    }
}

/// POST /api/skills/audit
pub async fn handle_api_skills_audit(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e;
    }
    
    let config = state.config.lock().unwrap();
    
    if let Some(name) = body.get("name").and_then(|v| v.as_str()) {
        match skills::audit_skill_by_name(name, &config) {
            Ok(report) => Json(serde_json::json!({
                "name": name,
                "files_scanned": report.files_scanned,
                "findings": report.findings,
                "is_clean": report.is_clean(),
                "summary": report.summary()
            })).into_response(),
            Err(e) => (StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": e.to_string()})))
                .into_response(),
        }
    } else {
        (StatusCode::BAD_REQUEST,
         Json(serde_json::json!({"error": "missing 'name' or 'source'"})))
            .into_response()
    }
}
```

#### 步骤 3：在 `src/gateway/mod.rs` 中注册路由

在 `run_gateway` 函数的路由注册部分（`Router::new()` 链式调用处）添加：

```rust
// src/gateway/mod.rs — 在 Router::new() 中添加

.route("/api/skills", get(api::handle_api_skills_list))
.route("/api/skills/install", post(api::handle_api_skills_install))
.route("/api/skills/audit", post(api::handle_api_skills_audit))
.route("/api/skills/:name", delete(api::handle_api_skills_remove))
```

> **注意路由顺序**：`/api/skills/install` 和 `/api/skills/audit` 应放在 `/api/skills/:name` 之前，避免 Axum 将 `install`/`audit` 匹配为 `:name` 参数。

---

## 第二部分：前端 — React Web Dashboard

### 2.1 需要创建/修改的文件

| 文件 | 操作 |
|------|------|
| `web/src/lib/api.ts` | 添加 skills API 调用函数 |
| `web/src/types/api.ts` | 添加 Skills 类型定义 |
| `web/src/pages/Skills.tsx` | **新建** Skills 页面组件 |
| `web/src/App.tsx` | 添加 Skills 路由 |
| `web/src/components/layout/Header.tsx` | 添加导航链接 |
| `web/src/lib/i18n.ts` | 添加国际化翻译 |

### 2.2 类型定义

```typescript
// web/src/types/api.ts — 新增

export interface SkillToolInfo {
  name: string;
  description: string;
  kind: string;
}

export interface SkillInfo {
  name: string;
  description: string;
  version: string;
  author: string | null;
  tools: SkillToolInfo[];
  prompts_count: number;
  location: string | null;
  always: boolean;
}

export interface SkillsListResponse {
  skills: SkillInfo[];
  open_skills_enabled: boolean;
  total: number;
}

export interface SkillAuditResult {
  files_scanned: number;
  findings: string[];
  is_clean: boolean;
}

export interface SkillInstallResponse {
  status: string;
  name: string;
  audit: SkillAuditResult;
}
```

### 2.3 API 调用层

```typescript
// web/src/lib/api.ts — 新增函数

export async function fetchSkills(): Promise<SkillsListResponse> {
  const res = await apiFetch('/api/skills');
  return res.json();
}

export async function installSkill(source: string): Promise<SkillInstallResponse> {
  const res = await apiFetch('/api/skills/install', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ source }),
  });
  if (!res.ok) {
    const err = await res.json();
    throw new Error(err.error || 'Install failed');
  }
  return res.json();
}

export async function removeSkill(name: string): Promise<{ deleted: boolean }> {
  const res = await apiFetch(`/api/skills/${encodeURIComponent(name)}`, {
    method: 'DELETE',
  });
  return res.json();
}

export async function auditSkill(name: string): Promise<SkillAuditResult> {
  const res = await apiFetch('/api/skills/audit', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name }),
  });
  return res.json();
}
```

### 2.4 Skills 页面组件

```tsx
// web/src/pages/Skills.tsx

import { useState, useEffect, useCallback } from 'react';
import { fetchSkills, installSkill, removeSkill, auditSkill } from '../lib/api';
import type { SkillInfo, SkillsListResponse } from '../types/api';

export default function Skills() {
  const [data, setData] = useState<SkillsListResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [installSource, setInstallSource] = useState('');
  const [installing, setInstalling] = useState(false);
  const [expandedSkill, setExpandedSkill] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setLoading(true);
      const result = await fetchSkills();
      setData(result);
      setError(null);
    } catch (e: any) {
      setError(e.message);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { load(); }, [load]);

  const handleInstall = async () => {
    if (!installSource.trim()) return;
    setInstalling(true);
    try {
      await installSkill(installSource.trim());
      setInstallSource('');
      await load(); // 刷新列表
    } catch (e: any) {
      setError(e.message);
    } finally {
      setInstalling(false);
    }
  };

  const handleRemove = async (name: string) => {
    if (!confirm(`Remove skill "${name}"?`)) return;
    try {
      await removeSkill(name);
      await load();
    } catch (e: any) {
      setError(e.message);
    }
  };

  const handleAudit = async (name: string) => {
    try {
      const result = await auditSkill(name);
      alert(
        result.is_clean
          ? `✅ ${name}: Clean (${result.files_scanned} files scanned)`
          : `⚠️ ${name}: ${result.findings.join(', ')}`
      );
    } catch (e: any) {
      setError(e.message);
    }
  };

  if (loading) return <div className="p-6">Loading skills...</div>;

  return (
    <div className="p-6 max-w-4xl mx-auto">
      <div className="flex items-center justify-between mb-6">
        <h1 className="text-2xl font-bold">Skills</h1>
        <span className="text-sm text-gray-500">
          {data?.total ?? 0} installed
          {data?.open_skills_enabled && ' · Open Skills enabled'}
        </span>
      </div>

      {error && (
        <div className="mb-4 p-3 bg-red-50 border border-red-200 rounded text-red-700 text-sm">
          {error}
          <button onClick={() => setError(null)} className="ml-2 underline">dismiss</button>
        </div>
      )}

      {/* 安装区域 */}
      <div className="mb-6 flex gap-2">
        <input
          type="text"
          value={installSource}
          onChange={(e) => setInstallSource(e.target.value)}
          placeholder="Git URL or local path (e.g. https://github.com/user/skill.git)"
          className="flex-1 px-3 py-2 border rounded text-sm"
          onKeyDown={(e) => e.key === 'Enter' && handleInstall()}
        />
        <button
          onClick={handleInstall}
          disabled={installing || !installSource.trim()}
          className="px-4 py-2 bg-blue-600 text-white rounded text-sm disabled:opacity-50"
        >
          {installing ? 'Installing...' : 'Install'}
        </button>
      </div>

      {/* Skills 列表 */}
      <div className="space-y-3">
        {data?.skills.map((skill) => (
          <div key={skill.name} className="border rounded-lg overflow-hidden">
            {/* 卡片头部 */}
            <div
              className="p-4 flex items-center justify-between cursor-pointer hover:bg-gray-50"
              onClick={() => setExpandedSkill(
                expandedSkill === skill.name ? null : skill.name
              )}
            >
              <div>
                <div className="flex items-center gap-2">
                  <span className="font-semibold">{skill.name}</span>
                  <span className="text-xs bg-gray-100 px-2 py-0.5 rounded">
                    v{skill.version}
                  </span>
                  {skill.always && (
                    <span className="text-xs bg-blue-100 text-blue-700 px-2 py-0.5 rounded">
                      always
                    </span>
                  )}
                </div>
                <p className="text-sm text-gray-500 mt-1">{skill.description}</p>
              </div>
              <div className="flex items-center gap-2">
                <span className="text-xs text-gray-400">
                  {skill.tools.length} tools · {skill.prompts_count} prompts
                </span>
                <span className="text-gray-400">
                  {expandedSkill === skill.name ? '▲' : '▼'}
                </span>
              </div>
            </div>

            {/* 展开详情 */}
            {expandedSkill === skill.name && (
              <div className="border-t px-4 py-3 bg-gray-50">
                {skill.author && (
                  <p className="text-sm text-gray-500 mb-2">Author: {skill.author}</p>
                )}
                {skill.location && (
                  <p className="text-xs text-gray-400 mb-2 font-mono">{skill.location}</p>
                )}

                {/* Tools 列表 */}
                {skill.tools.length > 0 && (
                  <div className="mb-3">
                    <p className="text-sm font-medium mb-1">Tools:</p>
                    <div className="space-y-1">
                      {skill.tools.map((tool) => (
                        <div key={tool.name} className="text-sm flex items-center gap-2">
                          <code className="bg-gray-200 px-1.5 py-0.5 rounded text-xs">
                            {tool.name}
                          </code>
                          <span className="text-xs bg-gray-100 px-1 rounded">{tool.kind}</span>
                          <span className="text-gray-500">{tool.description}</span>
                        </div>
                      ))}
                    </div>
                  </div>
                )}

                {/* 操作按钮 */}
                <div className="flex gap-2 mt-3">
                  <button
                    onClick={() => handleAudit(skill.name)}
                    className="px-3 py-1 text-sm border rounded hover:bg-gray-100"
                  >
                    🔍 Audit
                  </button>
                  <button
                    onClick={() => handleRemove(skill.name)}
                    className="px-3 py-1 text-sm border border-red-200 text-red-600 rounded hover:bg-red-50"
                  >
                    🗑 Remove
                  </button>
                </div>
              </div>
            )}
          </div>
        ))}

        {data?.skills.length === 0 && (
          <div className="text-center py-12 text-gray-400">
            No skills installed. Use the input above to install one.
          </div>
        )}
      </div>
    </div>
  );
}
```

### 2.5 路由注册

```tsx
// web/src/App.tsx — 添加路由

import Skills from './pages/Skills';

// 在 Routes 中添加：
<Route path="/skills" element={<Skills />} />
```

### 2.6 导航链接

```tsx
// web/src/components/layout/Header.tsx — 添加导航项

<NavLink to="/skills">Skills</NavLink>
```

### 2.7 国际化 (i18n)

```typescript
// web/src/lib/i18n.ts — 在翻译字典中添加

// English
skills: "Skills",
skills_installed: "installed",
skills_install: "Install",
skills_installing: "Installing...",
skills_remove: "Remove",
skills_audit: "Audit",
skills_no_skills: "No skills installed",
skills_open_enabled: "Open Skills enabled",

// 中文
skills: "技能",
skills_installed: "已安装",
skills_install: "安装",
skills_installing: "安装中...",
skills_remove: "删除",
skills_audit: "审计",
skills_no_skills: "暂无安装的技能",
skills_open_enabled: "开放技能已启用",
```

---

## 第三部分：关键实现细节

### 3.1 安全要点

Skills 系统有严格的安全审计机制，API 层必须保持一致：

1. **路径穿越防护** — `remove_skill` 必须拒绝包含 `..`、`/`、`\` 的名称，并验证 canonical path 在 skills 目录内（参考 `src/skills/mod.rs:938-951`）
2. **安装前审计** — 所有安装请求必须先通过 `audit_skill_directory()` 检查（参考 `src/skills/audit.rs`），审计会检查：
   - 符号链接 → 阻止
   - 脚本文件（.sh, .ps1, .bat）→ 标记高风险
   - 超过 512KB 的文本文件 → 拒绝
   - 不安全的 markdown 链接模式 → 标记
3. **认证保护** — 所有端点都需要通过 `require_auth` 验证 Bearer token

### 3.2 与现有 CLI 逻辑的关系

```
CLI: zeroclaw skills list
  └─→ handle_command() in src/skills/mod.rs
       └─→ load_skills_with_config()  ←──── API 也调用这个

CLI: zeroclaw skills install <source>
  └─→ handle_command()
       └─→ is_git_source() → install_git_skill / install_local_skill
            └─→ audit_skill_directory()  ←──── API 也调用这个

CLI: zeroclaw skills remove <name>
  └─→ handle_command()
       └─→ path validation + fs::remove_dir_all  ←──── API 复用同样的逻辑
```

核心原则是 **提取 → 复用**：将 `handle_command()` 中的逻辑提取为独立函数，CLI 和 API 共用。

### 3.3 现有代码中的关键行号参考

基于 DeepWiki 索引（commit f7fefd4b），关键源码位置：

| 功能 | 文件 | 行号 |
|------|------|------|
| Skill/SkillTool 结构体 | `src/skills/mod.rs` | 22-53 |
| SkillManifest (TOML 解析) | `src/skills/mod.rs` | 56-75 |
| load_skills_with_config | `src/skills/mod.rs` | 87-95 |
| load_skills_from_directory | `src/skills/mod.rs` | 217-295 |
| SKILL.md 解析 | `src/skills/mod.rs` | 565-632 |
| CLI handle_command | `src/skills/mod.rs` | ~635-950 |
| Git 安装 | `src/skills/mod.rs` | 635-677 |
| 本地安装 | `src/skills/mod.rs` | 775-828 |
| 删除 + 路径穿越防护 | `src/skills/mod.rs` | 938-951 |
| 安全审计入口 | `src/skills/audit.rs` | 25-53 |
| 审计检查逻辑 | `src/skills/audit.rs` | 108-211 |
| Gateway 路由注册 | `src/gateway/mod.rs` | 280-313 |
| API handler 模板 | `src/gateway/api.rs` | 72-572 |
| 认证中间件 | `src/gateway/api.rs` | 26-45 |
| Dashboard API 层 | `web/src/lib/api.ts` | 26-64 |
| Dashboard 路由 | `web/src/App.tsx` | 1-15 |

### 3.4 构建与测试

```bash
# 后端
cd zeroclaw
cargo build --release
cargo test --test skills_api  # 建议添加集成测试

# 前端
cd web
npm ci
npm run build  # 产物会被 gateway 的 static_files.rs 嵌入
```

前端构建后，需要重新编译 Rust 二进制（因为 dashboard 是嵌入在二进制中的）。

---

## 第四部分：推荐实施顺序

1. **后端 Step 1** — 在 `src/skills/mod.rs` 中提取公开函数 + 添加 `SkillInfo` 结构体
2. **后端 Step 2** — 先实现 `GET /api/skills` 端点并测试
3. **后端 Step 3** — 实现 `DELETE` 和 `POST /install`、`POST /audit`
4. **前端 Step 1** — 添加类型定义和 API 调用函数
5. **前端 Step 2** — 创建 Skills.tsx 页面（先只做列表展示）
6. **前端 Step 3** — 添加安装、删除、审计交互
7. **集成测试** — 端到端验证
8. **提交 PR** — 遵循项目的 CLA 和贡献指南
