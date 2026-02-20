# Web Dashboard 开发任务书

> **关联 PRD**: `03-web-dashboard-prd.md` v1.3
> **日期**: 2026-02-20
> **状态**: 待执行

---

## 执行总览

```
Step 0: 推送 main ──────────────────── git push
Step 1: workspace 修复 ─────────────── fix/workspace-config → PR → merge
Step 2: Phase 1 基础框架 ──────────── feat/web-dashboard-phase-1 → PR → merge
Step 3: Phase 2 聊天+提示词 ────────── feat/web-dashboard-phase-2 → PR → merge
Step 4: Phase 3 记忆+工具+调度 ────── feat/web-dashboard-phase-3 → PR → merge
Step 5: Phase 4 审计+可观测 ────────── feat/web-dashboard-phase-4 → PR → merge
Step 6: Phase 5 技能+配置+通道 ────── feat/web-dashboard-phase-5 → PR → merge
```

每个 Step 对应一个独立分支和 PR，合入后再开始下一个 Step。

---

## Step 0: 推送 main 分支

**目标**：将当前 94 个未推送的本地 commit 同步到 origin/main

**任务清单：**

- [ ] **0.1** 确认本地 main 干净：`git status` 无未提交变更
- [ ] **0.2** 推送到远程：`git push origin main`
- [ ] **0.3** 验证远程：`git log origin/main --oneline -5` 确认同步

**验收**：`git status` 显示 "Your branch is up to date with 'origin/main'"

---

## Step 1: Workspace 配置修复

**分支**：`fix/workspace-config`
**对应 PRD**：1.4 节（前置条件）

### 1.1 修复 ZEROCLAW_WORKSPACE 重复解析 (W-1)

**文件**：`src/config/schema.rs`

- [ ] 定位 `load_or_init()` 中对 `ZEROCLAW_WORKSPACE` 的提前解析（约 L1919）
- [ ] 移除该处理，统一由 `apply_env_overrides()`（约 L2038）处理
- [ ] 添加单元测试：验证 `ZEROCLAW_WORKSPACE` 仅在 `apply_env_overrides()` 生效

### 1.2 修复 serde(skip) 导致 TOML 配置静默忽略 (W-2)

**文件**：`src/config/schema.rs`

- [ ] 将 `workspace_dir` 的 `#[serde(skip)]`（L16）改为 `#[serde(skip_serializing)]`
- [ ] 将 `config_path` 的 `#[serde(skip)]`（L19）改为 `#[serde(skip_serializing)]`
- [ ] 或者：保留 skip 但在 TOML 解析时检测到这些字段存在时输出 warn 日志
- [ ] 选择方案后添加测试验证行为

### 1.3 修复 canonicalize() 静默回退 (W-3)

**文件**：`src/security/policy.rs`

- [ ] 定位 `canonicalize()` 的 `unwrap_or_else` 回退（约 L523-524）
- [ ] 改为：记录 `warn!` 日志 + 返回明确错误（或至少日志记录）
- [ ] 添加测试：验证非法路径不再静默降级

### 1.4 添加 workspace 启动校验 (W-4)

**文件**：`src/daemon/mod.rs` 或启动流程入口

- [ ] 在 daemon/gateway 启动阶段增加：
  - workspace 目录存在性检查
  - workspace 目录可写权限检查
  - 关键子目录（memory/、skills/ 等）存在性检查（不存在则创建）
- [ ] 校验失败时 `bail!` + 清晰错误消息
- [ ] 添加测试

### 1.5 验证与提交

- [ ] 运行 `cargo fmt --all -- --check`
- [ ] 运行 `cargo clippy --all-targets -- -D warnings`
- [ ] 运行 `cargo test`
- [ ] 提交：`fix(config): resolve workspace config issues (W-1 ~ W-4)`
- [ ] 推送并创建 PR → 合入 main

**验收**：所有现有测试通过 + 新增测试覆盖 4 个修复点

---

## Step 2: Phase 1 — 基础框架 + 状态总览

**分支**：`feat/web-dashboard-phase-1`
**对应 PRD**：Phase 1

### 2.1 后端：GET /status 端点

**文件**：`src/gateway/mod.rs`（或新建 `src/gateway/api/`）

- [ ] 设计 `StatusResponse` 结构体（版本、uptime、PID、provider、model、temperature、autonomy_level、memory_backend、组件健康、通道状态、workspace 信息）
- [ ] 实现 `GET /status` handler，从现有系统状态聚合数据
- [ ] 注册路由到 Axum router

### 2.2 后端：Bearer Token 认证中间件

**文件**：`src/gateway/mod.rs` 或新建 `src/gateway/auth.rs`

- [ ] 实现 Axum 中间件：验证 `Authorization: Bearer <token>` 头
- [ ] Token 与已配对的 token 比对
- [ ] 无效/缺失 token 返回 401 JSON 错误
- [ ] `/health` 和 `/pair` 不需要认证（保持现有行为）
- [ ] 所有新增管理端点应用该中间件

### 2.3 后端测试

- [ ] `GET /status` 返回正确结构的测试
- [ ] 认证中间件：无 token → 401 的测试
- [ ] 认证中间件：无效 token → 401 的测试
- [ ] 认证中间件：有效 token → 200 的测试
- [ ] 运行 `cargo test` + `cargo clippy`

### 2.4 前端：路由框架

**文件**：`learn-zeroclaw/web-ui/src/`

- [ ] 安装依赖：`react-router-dom`、`@tanstack/react-query`、`react-i18next`、`i18next`
- [ ] 创建 `src/layouts/DashboardLayout.tsx`：侧边栏 + 主内容区
- [ ] 创建侧边栏组件，包含 11 个导航项（图标 + 文字）
- [ ] 未实现的页面显示灰色 + "Coming Soon" 标签
- [ ] 侧边栏底部：语言切换 + 登出按钮
- [ ] 设置 `react-router-dom` 路由表（参照 PRD 4.2 节）
- [ ] 重构 `App.tsx`：从状态机改为路由容器
- [ ] 配对逻辑移入 `/pair` 路由，未认证时重定向

### 2.5 前端：i18n 基础设施

- [ ] 创建 `src/i18n/index.ts`：i18next 初始化
- [ ] 创建 `src/i18n/locales/en/common.json`
- [ ] 创建 `src/i18n/locales/zh/common.json`
- [ ] 检测优先级：localStorage → navigator.language → 默认 en
- [ ] 语言切换组件：点击切换，持久化到 localStorage
- [ ] 所有侧边栏文案使用 `t()` 翻译

### 2.6 前端：Dashboard 页面

- [ ] 创建 `src/pages/Dashboard.tsx`
- [ ] 状态卡片行：运行状态、当前模型、记忆后端、自治等级
- [ ] 组件健康列表：红绿灯 + 最后活跃时间 + 重启次数
- [ ] Quick Stats 区域：使用 "--" placeholder（6 个指标卡片）
- [ ] 数据获取：使用 `useQuery` + `refetchInterval: 10000`
- [ ] 创建 `src/i18n/locales/en/dashboard.json` 和 `zh/dashboard.json`

### 2.7 前端：配对页面适配

- [ ] 将现有 `PairingForm.tsx` 适配为 `/pair` 路由页面
- [ ] 配对成功后 `navigate('/')` 跳转 Dashboard
- [ ] 未认证访问其他页面时重定向到 `/pair`

### 2.8 前端：API 客户端扩展

**文件**：`src/lib/api.ts`

- [ ] 添加 `getStatus()` 方法
- [ ] 统一错误处理：401 → 清除 token → 跳转配对页
- [ ] 统一 Bearer Token 注入

### 2.9 前端测试

- [ ] 安装 `vitest` + `@testing-library/react` + `@testing-library/jest-dom`
- [ ] 配置 `vitest.config.ts`（或在 `vite.config.ts` 中配置）
- [ ] Dashboard 组件渲染测试：mock API 数据，验证卡片显示
- [ ] 侧边栏测试：验证所有导航项渲染、未实现页面灰显
- [ ] 语言切换测试：切换后文案正确变化
- [ ] 配对流程测试：未认证时重定向

### 2.10 验证与提交

- [ ] 后端：`cargo fmt && cargo clippy && cargo test`
- [ ] 前端：`npm run lint && npm test && npm run build`
- [ ] 手动验证：启动后端 + 前端，完成配对 → Dashboard 正常显示 → 中英文切换正常
- [ ] 提交、推送、创建 PR

**验收**：
- Dashboard 正常显示系统状态
- 侧边栏 11 个导航项可见，未实现页面灰显
- 中英文切换即时生效
- 前端测试全部通过

---

## Step 3: Phase 2 — 聊天增强 + 系统提示词管理

**分支**：`feat/web-dashboard-phase-2`
**对应 PRD**：Phase 2

### 3.1 后端：Webhook 响应扩展

**文件**：`src/gateway/mod.rs`

- [ ] 扩展 webhook handler 响应结构，增加 `tool_calls`、`tokens_used`、`cost_usd`、`model`、`session_id` 字段
- [ ] 保持向后兼容：旧客户端仍可正常使用 `response` 字段
- [ ] 从 Agent loop 结果中提取工具调用详情

### 3.2 后端：Prompts API

**文件**：新建 `src/gateway/api/prompts.rs`（或在 gateway 中添加）

- [ ] `GET /prompts`：列出所有 8 个提示词文件的信息（名称、大小、字符数、摘要、更新时间、角色）
- [ ] `GET /prompts/:filename`：返回单个文件完整内容
- [ ] `PUT /prompts/:filename`：全量覆盖文件内容
- [ ] `GET /prompts/preview`：按组装顺序合并所有文件，返回完整提示词
- [ ] 文件名白名单校验：仅允许 `AGENTS.md, SOUL.md, TOOLS.md, IDENTITY.md, USER.md, HEARTBEAT.md, MEMORY.md, BOOTSTRAP.md`
- [ ] 非法文件名返回 403
- [ ] UTF-8 内容校验

### 3.3 后端测试

- [ ] webhook 扩展响应格式测试
- [ ] prompts 列表 API 测试
- [ ] prompts 单文件读取测试
- [ ] prompts 更新 + 白名单校验测试
- [ ] prompts 预览合成测试

### 3.4 前端：Chat 增强

**文件**：`src/components/Chat.tsx` + 新组件

- [ ] 工具调用卡片组件：折叠/展开、工具名、输入参数、输出、耗时、颜色区分
- [ ] 模型切换器下拉（顶栏）+ Temperature 滑块
- [ ] 会话管理：新建/切换/清除会话
- [ ] Markdown 渲染 + 代码语法高亮（使用 `react-markdown` + `rehype-highlight` 或类似方案）
- [ ] 每条回复显示 Token 用量和费用
- [ ] 复制按钮、重新生成按钮
- [ ] `src/i18n/locales/en/chat.json` 和 `zh/chat.json`

### 3.5 前端：Prompts 页面

**文件**：新建 `src/pages/Prompts.tsx` + 子组件

- [ ] 文件列表：按组装顺序排列的提示词卡片
- [ ] 每张卡片：文件名、职责标签、字符数进度条、内容预览
- [ ] 顶部汇总：总字符数、identity_format
- [ ] 编辑弹窗（Modal）：等宽字体 textarea、实时字符计数、20K 上限进度条
- [ ] 超过 20K 字符显示橙色警告
- [ ] 保存前确认对话框
- [ ] MEMORY.md 编辑时额外提示（Agent 自行维护）
- [ ] 组装预览弹窗：完整提示词只读展示 + Copy All
- [ ] BOOTSTRAP.md 未创建时显示 [Create] 按钮
- [ ] `src/i18n/locales/en/prompts.json` 和 `zh/prompts.json`

### 3.6 前端测试

- [ ] Chat 消息渲染测试（含工具卡片）
- [ ] 工具卡片展开/折叠测试
- [ ] Prompts 列表渲染测试
- [ ] Prompts 编辑保存流程测试（含确认对话框）
- [ ] Prompts 预览渲染测试

### 3.7 验证与提交

- [ ] 后端 + 前端全套检查
- [ ] 手动验证：聊天 → 工具调用可视化 → 提示词编辑 → 预览
- [ ] 提交、推送、创建 PR

**验收**：
- 聊天界面显示工具调用详情
- 可编辑系统提示词并预览组装效果
- 所有测试通过

---

## Step 4: Phase 3 — 记忆、工具与调度

**分支**：`feat/web-dashboard-phase-3`
**对应 PRD**：Phase 3

### 4.1 后端：Memory API

- [ ] `GET /memory`：查询记忆（分页 + 分类/session/关键词筛选）
- [ ] `GET /memory/:id`：单条记忆
- [ ] `POST /memory`：新建
- [ ] `PUT /memory/:id`：更新
- [ ] `DELETE /memory/:id`：删除
- [ ] `GET /memory/stats`：各分类条数统计
- [ ] 对接现有 Memory trait 实现

### 4.2 后端：Tools API

- [ ] `GET /tools`：已注册工具列表（含启用状态、分类、安全属性摘要）
- [ ] `GET /tools/:name`：单个工具详情（参数 schema + 安全策略）
- [ ] `GET /tools/stats`：各工具调用统计（从 Observer 数据聚合）
- [ ] 对接现有 Tool trait 的 `spec()` 方法获取 schema

### 4.3 后端：Cron Jobs API

- [ ] `GET /cron/jobs`：任务列表
- [ ] `POST /cron/jobs`：创建（名称、cron 表达式、命令、类型、投递配置）
- [ ] `PATCH /cron/jobs/:id`：更新（含暂停/恢复）
- [ ] `DELETE /cron/jobs/:id`：删除
- [ ] `GET /cron/jobs/:id/runs`：执行历史
- [ ] 对接现有 cron 模块

### 4.4 后端测试

- [ ] Memory CRUD 全流程测试
- [ ] Memory 筛选（分类、关键词、分页）测试
- [ ] Tools 列表和详情测试
- [ ] Tools stats 聚合测试
- [ ] Cron jobs CRUD + 暂停/恢复测试

### 4.5 前端：Memory 页面

- [ ] 搜索栏（语义搜索）+ 分类 Tab 筛选
- [ ] 记忆卡片列表：key、content 预览、分类标签、时间、score
- [ ] 新建记忆表单
- [ ] 编辑弹窗
- [ ] 删除确认对话框
- [ ] 分页
- [ ] i18n

### 4.6 前端：Tools 页面

- [ ] 顶部：工具总数 + 限流进度条
- [ ] 分类 Tab 筛选
- [ ] 工具卡片列表（名称、分类、启用状态、安全属性、统计）
- [ ] 禁用工具灰显 + 原因
- [ ] 详情弹窗（参数 Schema、安全策略、统计图表）
- [ ] 限流 > 80% 时橙色告警
- [ ] i18n

### 4.7 前端：Scheduler 页面

- [ ] 任务卡片列表（名称、类型、cron 表达式 + 人类可读描述、下次执行、上次状态）
- [ ] 新建任务表单（含 cron 表达式可视化预览）
- [ ] 暂停/恢复、编辑、删除操作
- [ ] 展开查看执行历史
- [ ] i18n

### 4.8 前端测试

- [ ] Memory 搜索/CRUD 测试
- [ ] Tools 列表/详情/限流告警测试
- [ ] Scheduler 创建/暂停/历史测试

### 4.9 验证与提交

- [ ] 全套检查 + 手动验证
- [ ] 提交、推送、创建 PR

**验收**：
- 记忆可搜索/浏览/增删改
- 工具列表显示完整信息和统计
- 定时任务可创建/暂停/查看历史

---

## Step 5: Phase 4 — 审计与可观测性

**分支**：`feat/web-dashboard-phase-4`
**对应 PRD**：Phase 4

### 5.1 后端：Audit Logs API

- [ ] `GET /audit/logs`：分页查询 + 类型/风险/时间范围筛选
- [ ] 对接现有审计系统数据
- [ ] 敏感命令输出截断（前 1KB）

### 5.2 后端：Metrics API

- [ ] `GET /metrics`：聚合 Observer 数据（LLM 请求数、token 用量、费用、延迟、工具调用、错误数、活跃会话数）
- [ ] 支持时间范围参数（`?since=...`）

### 5.3 后端测试

- [ ] Audit logs 分页/筛选测试
- [ ] Metrics 聚合正确性测试

### 5.4 前端：Audit Log 页面

- [ ] 时间线样式日志流
- [ ] 筛选器：事件类型、风险等级、时间范围
- [ ] PolicyViolation / AuthFailure 高亮
- [ ] 日志条目展开查看完整 JSON
- [ ] 无限滚动加载
- [ ] i18n

### 5.5 前端：Metrics 页面

- [ ] 引入 `recharts`
- [ ] 4 个核心图表：LLM 请求量、Token 用量、费用累计、平均延迟
- [ ] 时间范围切换：1h / 6h / 24h / 7d
- [ ] 工具调用分布柱状图（频率 + 成功率）
- [ ] 30 秒自动刷新
- [ ] i18n

### 5.6 前端：Dashboard Quick Stats 回填

- [ ] 将 Dashboard 的 "--" placeholder 替换为 `/metrics` 真实数据
- [ ] 添加 `useQuery` 调用获取 metrics 数据

### 5.7 SSE 评估（可选）

- [ ] 评估 Audit Log 实时推送是否需要 SSE
- [ ] 如需要：后端实现 `GET /events` SSE 端点
- [ ] 如不需要：保持 polling，记录评估结论

### 5.8 前端测试

- [ ] Audit 筛选/展开测试
- [ ] Metrics 图表渲染测试
- [ ] Dashboard Quick Stats 数据显示测试

### 5.9 验证与提交

- [ ] 全套检查 + 手动验证
- [ ] 提交、推送、创建 PR

**验收**：
- 审计日志可按类型/风险/时间筛选
- Metrics 图表正确渲染
- Dashboard Quick Stats 显示真实数据

---

## Step 6: Phase 5 — 技能、配置与通道

**分支**：`feat/web-dashboard-phase-5`
**对应 PRD**：Phase 5

### 6.1 后端：Skills API

- [ ] `GET /skills`：已安装技能列表（名称、版本、来源、标签、工具数、提示词数）
- [ ] `GET /skills/:name`：技能详情（含 manifest 内容）
- [ ] `POST /skills/install`：从 URL 或本地路径安装
- [ ] `DELETE /skills/:name`：卸载
- [ ] 对接现有 skills 模块

### 6.2 后端：SkillForge API

- [ ] `GET /skills/forge`：SkillForge 状态 + 最近扫描结果 + 候选列表
- [ ] `POST /skills/forge/scan`：手动触发扫描
- [ ] 对接现有 skillforge 模块

### 6.3 后端：Config API

- [ ] `GET /config`：返回当前配置（API Key 等敏感字段用 `***` 替代）
- [ ] `PATCH /config`：热更新白名单字段（default_model、default_temperature、autonomy.level、memory.auto_save、heartbeat.enabled、heartbeat.interval_minutes）
- [ ] 非白名单字段返回 400 + 提示需要重启

### 6.4 后端：Channels API

- [ ] `GET /channels`：所有通道状态（名称、连接状态、最后活跃时间、重启次数、消息统计）
- [ ] 对接现有 Channel trait 的 `health_check()` 等方法

### 6.5 后端测试

- [ ] Skills CRUD 测试
- [ ] SkillForge 状态/扫描测试
- [ ] Config 脱敏测试 + 热更新白名单测试
- [ ] Channels 状态查询测试

### 6.6 前端：Skills 页面

- [ ] Installed Tab：技能卡片（名称、版本、来源、标签、工具数/提示词数）
- [ ] 技能详情弹窗（metadata + manifest 原文 + 工具列表 + 提示词列表）
- [ ] 安装弹窗（URL/路径输入）
- [ ] 卸载确认对话框
- [ ] SkillForge Tab：引擎状态、扫描摘要、手动扫描按钮
- [ ] 候选列表分组（Auto Integrated / Needs Review / Skipped）
- [ ] 候选操作：Integrate / Dismiss
- [ ] i18n

### 6.7 前端：Channels 页面

- [ ] 通道卡片列表（名称 + 图标、状态灯、最后活跃、重启次数、消息统计）
- [ ] 未配置通道灰显 + 配置提示
- [ ] i18n

### 6.8 前端：Settings 页面

- [ ] 配置分组：模型设置、自治等级、记忆设置、心跳设置（可热更新）
- [ ] 安全配置、网关配置（只读，标注"需重启"）
- [ ] 可热更新字段有保存按钮
- [ ] API Key 等用 `***` 脱敏
- [ ] i18n

### 6.9 前端：全局搜索

- [ ] `Ctrl/Cmd + K` 唤出搜索面板
- [ ] 搜索范围：记忆条目 + 导航命令
- [ ] 搜索结果列表 + 键盘导航

### 6.10 前端测试

- [ ] Skills 安装/卸载测试
- [ ] SkillForge 扫描/候选审核测试
- [ ] Channels 状态显示测试
- [ ] Settings 热更新/只读区分测试
- [ ] 全局搜索唤出/导航测试

### 6.11 最终验证

- [ ] 后端：`cargo fmt && cargo clippy && cargo test`
- [ ] 前端：`npm run lint && npm test && npm run build`
- [ ] 全功能手动验证：11 个页面全部可用
- [ ] 中英文切换所有页面正确
- [ ] 移动端布局检查（375px）
- [ ] 生产构建性能：首屏 < 2 秒
- [ ] 提交、推送、创建 PR

**验收**：
- 所有 11 个页面功能完整
- PRD 10 节验收标准全部满足
- 完整的 Web Dashboard 交付

---

## 风险与缓解

| 风险 | 影响 | 缓解措施 |
|------|------|---------|
| 后端 API 数据聚合复杂度高 | 延期 | 优先实现简单字段，复杂聚合标记为 Phase 2 回填 |
| Observer 数据不足以支撑 Metrics | Metrics 图表为空 | Phase 4 先实现端点，数据不足时显示 "Insufficient data" |
| Skills 安装涉及文件系统操作 | 安全风险 | 后端严格校验路径、沙箱隔离 |
| 现有 Gateway 路由结构需重构 | 影响现有功能 | 新端点使用独立 router group，不修改现有 handler |

---

## 回滚策略

- 每个 Phase 是独立 PR，可独立 revert
- 前端路由设计保证未实现页面不影响已实现功能（灰显 + Coming Soon）
- 后端新增端点不修改现有端点行为，revert 不影响已有功能
- 数据库 schema 变更（如有）需包含 rollback migration

---

## 检查清单模板

每个 Phase PR 提交前对照：

```markdown
## PR 检查清单
- [ ] 后端：cargo fmt + clippy + test 通过
- [ ] 前端：lint + test + build 通过
- [ ] 新增 API 有 Bearer Token 认证
- [ ] 新增页面有中英文翻译
- [ ] 破坏性操作有确认对话框
- [ ] 移动端布局正常（至少 375px）
- [ ] 无敏感信息泄露（API Key 脱敏）
- [ ] PR 描述包含：变更内容、非目标、风险、回滚方案
```
