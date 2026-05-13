# 2026-05-13 Worklog: Channel Dashboard And RPi Deployment

## 背景

本次工作同时覆盖两条线：

1. Web 仪表盘 `频道` 页签加载失败，前端报错 `Unexpected token '<'`。
2. 树莓派部署链路需要补充实战记录，并统一默认用户、配置字段与前端资源路径。

## 问题定位过程

### 1. 仪表盘频道页排障

- 先从前端定位到 `Dashboard` 页面中的 `ChannelsTab()`，确认它会调用 `GET /api/channels`。
- 再回到 gateway 路由表检索，确认后端没有注册 `/api/channels`。
- 继续检查静态资源回退逻辑，确认未命中的 `GET` 会走 SPA fallback 并返回 `index.html`。
- 结合前端 `apiFetch()` 的 `response.json()` 解析逻辑，锁定真实根因：
  - 前端请求 `/api/channels`
  - 后端没有该接口
  - gateway 返回 HTML
  - 前端把 HTML 当 JSON 解析，触发 `Unexpected token '<'`

### 2. 扩大范围后的设计决策

- 用户选择的不只是补一个最小接口，而是同时补齐“真实频道统计”。
- 检查后确认现有仓库没有稳定的“每频道消息数 / 最后消息时间”读模型。
- 评估后决定不改 `Channel` trait，也不逐个侵入具体 adapter。
- 最终采用：
  - 在 `quantclaw-infra` 新增独立 `ChannelStatsStore`
  - 在 `quantclaw-channels` 的公共编排路径统一记账
  - 在 `quantclaw-gateway` 聚合配置、统计和 runtime health 输出 `/api/channels`

### 3. 树莓派部署链路梳理

- 对照已有部署脚本、安装脚本、配置模板和指南，发现实际使用约束已经发生偏移：
  - SSH/安装默认用户应为 `quant`，而不是 `pi`
  - Rust 服务目录应为 `~/quantclaw_rust_app`
  - 前端资源路径应统一到 `/usr/local/share/quantclaw/web/dist`
  - 配置模板里的旧字段 `challenge_delivery` 已需要迁移为 `method`
- 将这些结论同步写入脚本、配置模板和文档，减少后续重复踩坑

## 实施内容

### A. 频道页修复与统计能力

- 新增 `crates/quantclaw-infra/src/channel_stats.rs`
  - SQLite/WAL 持久化频道级聚合统计
  - 只保存频道级计数和时间戳，不保存 sender、消息正文等敏感信息
- 修改 `crates/quantclaw-infra/src/lib.rs`
  - 导出 `channel_stats` 模块
- 修改 `crates/quantclaw-channels/src/orchestrator/mod.rs`
  - 启动时初始化 `ChannelStatsStore`
  - 在统一入站路径记录 inbound
  - 在成功出站路径记录 outbound
  - 同步补 `ObserverEvent::ChannelMessage`
- 修改 `crates/quantclaw-gateway/src/api.rs`
  - 新增 `/api/channels` 处理逻辑
  - 将配置、统计、runtime health 聚合为前端现有 `ChannelDetail` 结构
- 修改 `crates/quantclaw-gateway/src/lib.rs`
  - 注册 `GET /api/channels`
- 增加 gateway 回归测试
  - 覆盖频道统计聚合与返回结构

### B. 树莓派部署与文档更新

- 修改 `scripts/build-release-aarch64.ps1`
  - 默认安装用户改为 `quant`
  - 发布前端目录改为 `/usr/local/share/quantclaw/web/dist`
- 修改 `scripts/build-release-aarch64.sh`
  - 与 PowerShell 版本保持一致
- 修改 `scripts/deploy-rpi.ps1`
  - 默认 SSH 用户改为 `quant`
  - 修正资源检查脚本引号形式
- 修改 `scripts/deploy-rpi.sh`
  - 默认 SSH 用户改为 `quant`
- 修改 `scripts/rpi-config.toml`
  - 将旧字段 `challenge_delivery` 改为 `method`
  - 显式补充 `host = "0.0.0.0"`
  - 指定 `web_dist_dir = "/usr/local/share/quantclaw/web/dist"`
- 修改 `scripts/README.md`
  - 增补配网目录冲突与配置字段兼容提醒
- 修改 `BUILD_AARCH64_GUIDE.md`
  - 补入 2026-05-11 实战对照记录、关键命令、迁移要点和验收标准

## 结果

### 频道页结果

- 仪表盘 `频道` 页不再依赖一个不存在的后端路由。
- gateway 现在提供真实的 `/api/channels` JSON 接口。
- 频道数据不再是纯占位，而是来自独立的持久化聚合统计。
- 统计维度保持在“频道级”，避免引入新的隐私暴露面。

### 树莓派部署结果

- Windows / Shell 两条 aarch64 打包与部署链路的默认用户统一为 `quant`。
- 前端静态资源复制路径与 gateway 配置路径统一为 `/usr/local/share/quantclaw/web/dist`。
- 配置模板已避免继续使用已弃用字段，降低首次部署后服务反复重启的概率。
- 文档中补充了这次实战的可复用命令和验收标准。

## 验证情况

- 已对本次直接编辑的核心代码文件执行 IDE 诊断检查，未发现新增诊断错误。
- 尝试执行：
  - `cargo fmt --all`
  - `cargo test -p quantclaw-infra channel_stats -- --nocapture`
  - `cargo test -p quantclaw-gateway api_channels_returns_aggregated_channel_stats -- --nocapture`
- 以上命令在当前 Trae 本地沙箱环境下被外部进程问题阻断，表现为：
  - `trae-sandbox` 子进程 panic
  - `called Result::unwrap() on an Err value: Os { code: 0, kind: Uncategorized, message: "操作成功完成。" }`
- 结论：
  - 当前无法在该环境内完成完整 Rust 编译验证
  - 但源码级静态诊断未见新增错误

## 涉及文件

- `BUILD_AARCH64_GUIDE.md`
- `crates/quantclaw-channels/src/orchestrator/mod.rs`
- `crates/quantclaw-gateway/src/api.rs`
- `crates/quantclaw-gateway/src/lib.rs`
- `crates/quantclaw-infra/src/lib.rs`
- `crates/quantclaw-infra/src/channel_stats.rs`
- `scripts/README.md`
- `scripts/build-release-aarch64.ps1`
- `scripts/build-release-aarch64.sh`
- `scripts/deploy-rpi.ps1`
- `scripts/deploy-rpi.sh`
- `scripts/rpi-config.toml`

## 后续建议

- 在非 Trae 沙箱环境下重新执行：
  - `cargo fmt --all -- --check`
  - `cargo test`
- 进入树莓派实机后，按文档中的验收命令再次确认：
  - 服务状态
  - 监听端口
  - `web_dist_dir`
  - Web 配置页是否正常显示
