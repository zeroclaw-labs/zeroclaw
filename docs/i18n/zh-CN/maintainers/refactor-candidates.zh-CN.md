# 重构候选

`src/` 中最大的源文件，按严重程度排名。每个文件在单个文件中完成多个任务，损害了可读性、可测试性和合并冲突频率。

| 文件                   | 行数  | 问题                                   |
| ---------------------- | ----- | -------------------------------------- |
| `config/schema.rs`     | 7,647 | 整个系统的所有配置结构体都在一个文件中 |
| `onboard/wizard.rs`    | 7,200 | 整个引导流程在一个类似函数的大块中     |
| `channels/mod.rs`      | 6,591 | 渠道工厂 + 共享逻辑 + 所有接线         |
| `agent/loop_.rs`       | 5,599 | 整个代理编排循环                       |
| `channels/telegram.rs` | 4,606 | 单个渠道实现不应该这么大               |
| `providers/mod.rs`     | 2,903 | 提供商工厂 + 共享转换逻辑              |
| `gateway/mod.rs`       | 2,777 | HTTP 服务器设置 + 中间件 + 路由        |

## 附加说明

- `tools/mod.rs`（635 行）有一个 13 参数的 `all_tools_with_runtime()` 工厂函数，随着工具数量增长会变得更糟。考虑使用注册表/构建器模式。
- `security/policy.rs`（2,338 行）混合了策略定义、操作跟踪和验证 —— 可以按关注点拆分。
- `providers/compatible.rs`（2,892 行）和 `providers/gemini.rs`（2,142 行）作为单个提供商实现来说太大了 —— 可能混合了 HTTP 客户端逻辑、响应解析和工具转换。

### 放错位置的模块：`channels/tts.rs` → `tools/`

`channels/tts.rs`（642 行，在 PR #2994 中合并）是一个多提供商 TTS 合成系统。它不是一个渠道 —— 它没有实现 `Channel` 也没有提供双向消息接口。TTS 是代理调用以产生音频输出的能力，符合 `Tool` 特征（`src/tools/traits.rs`）。它应该被移动到 `src/tools/tts.rs`，并实现对应的 `Tool`，其配置类型从 `schema.rs` 的 `channels` 部分提取到 `[tools.tts]` 配置命名空间。合并时，该模块没有集成到任何调用代码中（重新导出带有 `#[allow(unused_imports)]`），因此此移动对运行时没有影响。

---

## 最佳实践审计发现

来自通用 Rust/Python 最佳实践评审的发现（非项目特定约定）。

### 严重：生产代码中的 `.unwrap()`（约 2,800 处）

`.unwrap()` 出现在 I/O 路径、序列化和安全敏感模块中，超出了测试代码范围。示例：

```rust
// cost/tracker.rs
writeln!(file, "{}", serde_json::to_string(&old_record).unwrap()).unwrap();
file.sync_all().unwrap();
```

Rust 最佳实践：使用 `.context("msg")?` 或显式处理错误。每个 unwrap 都是瞬态失败时潜在的运行时 panic。

### 严重：生产路径中的 `panic!`（28+ 处）

提供商、配对和 CLI 路由使用 `panic!` 而非返回错误：

```rust
// providers/bedrock.rs
panic!("Expected ToolResult block");
// security/pairing.rs
panic!("Generated 10 pairs of codes and all were collisions — CSPRNG failure");
```

这些应该是 `bail!()` 或类型化错误变体 —— panic 是不可恢复的，会导致进程崩溃。

### 严重：全局 clippy 抑制（全局 32+ 个 lint）

`main.rs` 和 `lib.rs` 在 crate 级别抑制了 `too_many_lines`、`similar_names`、`dead_code`、`missing_errors_doc` 等许多 lint。这会隐藏新出现的违规。最佳实践：在函数级别抑制并附带理由注释，而非全局抑制。

### 高：静默错误吞吃（对 Result 使用 `let _ = ...`，30+ 处）

网关、WebSocket 和技能同步路径静默丢弃 `Result` 值：

```rust
let _ = state.event_tx.send(serde_json::json!({...})).await;
let _ = sender.send(Message::Text(err.to_string().into())).await;
let _ = mark_open_skills_synced(&repo_dir);
```

至少应该在失败时记录 `tracing::warn!`。静默丢弃使得分布式调试几乎不可能。

### 高：上帝结构体 —— 带有 30+ 字段的 `Config`

每个需要任何配置的子系统都必须持有整个 `Config` 结构体，造成隐式耦合和臃肿的测试设置。最佳实践：传递窄配置切片或特征绑定的配置对象。

### 高：安全代码未隔离

Shell 命令验证（300+ 行引号感知解析）、webhook 签名验证和配对逻辑嵌入在大型多用途文件中，而非隔离模块。这增加了安全审计的复杂性，并增加了无关变更导致回归的风险。

### 中：过多的 `.clone()`（约 1,227 处）

认证/令牌刷新路径在每个分支上克隆大型结构体。令牌访问等热点路径可以使用 `Cow<'_>` 或 `Arc` 而非完整克隆。

### 中：测试深度 —— 大部分是冒烟测试

存在 193 个测试模块（良好的结构覆盖），但大多数是简单的值断言。缺失：

- 解析器/验证器的基于属性的测试
- 多模块流程的集成测试
- Shell 命令解析器的模糊测试（安全表面）
- 网络依赖路径的基于模拟的测试

### 中：依赖数量（82 个直接依赖）

项目声称以大小优化为目标（`opt-level = "z"`、`lto = "fat"`），同时积累了重量级可选依赖，如 `matrix-sdk`（完整 E2EE 加密）和 `probe-rs`（50+ 个传递依赖）。大小目标和功能广度之间的矛盾尚未解决。

### 低：无安全注释的 `unsafe`

`src/service/mod.rs` 中有两处 `libc::getuid()` 的 `unsafe` 使用 —— 没有 `// SAFETY:` 注释。可以使用 `nix` crate 的安全包装器替代。

### 低：Python 代码质量

`python/` 子树的类型提示很少，关键函数没有 docstring，也没有参数化测试。与 Rust 侧的严谨性不一致。

### 低：极简的 `rustfmt.toml`

仅设置了 `edition = "2021"`。对于这种规模的项目，配置 `max_width`、`imports_granularity`、`group_imports` 可以在贡献者数量增长时强制一致性。

### 已解决：CI/CD 安全加固（P1/P2）

~~第三方操作固定到可变标签；发布工作流被授予过宽的写入权限；分支保护没有复合门控作业；每个 PR 都从源代码编译安全工具。~~

**已在 `cicd-best-practices` 分支修复：**

- 所有第三方操作都固定到 SHA（P1）
- 发布工作流权限按作业范围限定（P1）
- PR 检查中添加了复合 `Gate` 作业（P2）
- 通过预构建二进制安装安全工具（P2）

## 优先级建议

1. **将非测试代码中的 unwrap/panic 替换为** 正确的错误传播 —— 对稳定性影响最大。
2. **拆分上帝模块** —— 从 `channels/mod.rs` 中提取运行时编排，隔离安全解析，将 `Config` 拆分为子配置。
3. **移除全局 clippy 抑制** —— 逐个修复违规或添加带理由的逐项目 `#[allow]`。
4. **将 Result 上的 `let _ =` 替换为** 至少 `tracing::warn!` 日志。
5. **为安全表面解析器添加基于属性/模糊测试**（Shell 命令验证、webhook 签名）。

---

## 延后的结构重构

项目清理过程中延后的变更。每个条目包含理由和范围。

### 将 `src/sop/` 重命名为 `src/runbooks/`

**原因：** "SOP" 术语过重，不能传达模块的作用。"Runbooks" 是带有审批门控的触发器驱动自动化流程的行业标准术语。

**范围：** 重命名模块（`src/sop/` → `src/runbooks/`），更新配置键（`[sop]` → `[runbooks]`）、CLI 子命令（`jhedaiclaw sop` → `jhedaiclaw runbook`）、所有内部类型（`Sop*` → `Runbook*`）、文档（`docs/sop/` → 匹配新结构）以及 CLAUDE.md 中的引用。

### 将国际化文档整合到 `docs/i18n/<语言区域>/`

**原因：** 越南语翻译目前存在于三个位置：`docs/i18n/vi/`（根据 CLAUDE.md 规范）、`docs/vi/`（有 17 个文件分歧的过时副本）和 `docs/*.vi.md`（5 个分散的后缀文件）。其他语言区域（zh-CN、ja、ru、fr）的 SUMMARY + README 文件分散在 `docs/` 根目录。

**计划：**

- 保留 `docs/i18n/vi/` 作为规范版本；删除 `docs/vi/`（过时副本）
- 将 `docs/*.vi.md` 文件移动到 `docs/i18n/vi/` 下的对应路径
- 将 `docs/SUMMARY.*.md` 和 `docs/README.*.md` 移动到 `docs/i18n/<语言区域>/`
- 创建 `docs/i18n/{zh-CN,ja,ru,fr}/` 目录，包含其 README + SUMMARY
- 根目录 `README.*.md` 文件保留（GitHub 约定）
- 英文文档重构完成后，更新 `docs/i18n/vi/` 内部结构以匹配新的英文文档布局

### TODO：模糊测试 —— 将存根升级为真实覆盖

**当前状态：** `fuzz/fuzz_targets/` 中存在 5 个模糊测试目标，但只有 `fuzz_command_validation` 测试真实的 JhedaiClaw 代码。其他 4 个（`fuzz_config_parse`、`fuzz_tool_params`、`fuzz_webhook_payload`、`fuzz_provider_response`）仅模糊测试 `serde_json::from_str::<Value>` 或 `toml::from_str::<Value>` —— 它们测试第三方 crate 内部，而非 JhedaiClaw 逻辑。

**将现有存根连接到真实代码路径：**

- `fuzz_config_parse`：反序列化为 `Config`，而非 `toml::Value`
- `fuzz_tool_params`：通过实际的 `Tool::execute` 输入验证
- `fuzz_webhook_payload`：通过 webhook 签名验证 + 正文解析
- `fuzz_provider_response`：解析为实际的提供商响应类型（Anthropic、OpenAI 等）

**为安全表面添加缺失的目标：**

- Shell 命令解析器（引号感知解析，不只是 `validate_command_execution`）
- 凭证清理（`scrub_credentials` —— 在 #3024 中已经出现过 UTF-8 边界 panic）
- 配对代码生成/验证
- 域名匹配器
- 提示防护评分
- 泄露检测器正则表达式

**基础设施改进：**

- 添加种子语料库（`fuzz/corpus/<目标>/`），包含已知良好和边界情况输入；提交到仓库
- 考虑使用 `Arbitrary` 派生进行结构化模糊测试，而非原始 `&[u8]`
- 设置计划 CI 模糊测试（每日/每周）—— OSS-Fuzz 对开源项目免费
- 使用 `cargo fuzz coverage <目标>` 从语料库运行生成 lcov 报告，跟踪模糊测试实际覆盖的代码路径
- 将崩溃工件（`fuzz/artifacts/<目标>/`）作为 Issue 跟踪

### TODO：`e2e-testing` 分支的测试基础设施跟进

测试重构工作质量评审期间发现的问题。

**1. ~~运行器文件中的 `#[path]` 属性模式~~（已解决）**

~~运行器文件使用 `#[path]` 属性作为 E0761 的变通方案。~~ 已修复：运行器文件重命名为 `test_component.rs` 等，目录使用标准 `mod.rs` 文件。`Cargo.toml` 的 `[[test]]` 条目已更新以匹配。`cargo test --test component` 命令不变。

**2. 死基础设施：`TestChannel`、`TraceLlmProvider`、追踪夹具、`verify_expects()`**

这些是作为脚手架构建的，但没有使用者：

- `tests/support/mock_channel.rs`（`TestChannel`）—— 计划用于渠道驱动的系统测试，但代理没有公共的渠道驱动循环 API，因此系统测试直接使用 `agent.turn()`。
- `tests/support/mock_provider.rs`（`TraceLlmProvider`）—— 重放 JSON 夹具追踪，但没有测试加载或运行夹具。
- `tests/fixtures/traces/*.json`（3 个文件）—— 从未被任何测试加载。
- `tests/support/assertions.rs`（`verify_expects()`）—— 从未被调用。

要么编写使用这些基础设施的测试，要么移除它们以避免死代码混淆。

**3. 网关组件测试与现有 `whatsapp_webhook_security.rs` 重叠**

`tests/component/gateway.rs` 中有 6 个针对 `verify_whatsapp_signature()` 的 HMAC 签名验证测试 —— 与 `tests/component/whatsapp_webhook_security.rs` 中的 8 个测试测试同一个函数。只有 3 个网关常量测试（`MAX_BODY_SIZE`、`REQUEST_TIMEOUT_SECS`、`RATE_LIMIT_WINDOW_SECS`）提供了真正的新覆盖。考虑将签名测试合并到一个文件中，或从 `gateway.rs` 中删除重复项。

### 4. 安全组件测试仅配置 —— 没有行为覆盖

10 个安全测试仅验证配置默认值和 TOML 序列化（`AutonomyConfig::default()`、`SecretsConfig`、往返）。它们不测试安全*行为*（策略执行、凭证清理、操作速率限制），因为 `src/security/` 是 `pub(crate)` 的。`security_config_debug_does_not_leak_api_key` 测试是无操作的 —— 它检查泄露，但失败时没有断言（只有注释）。要获得真实的行为覆盖，可以：

- 让目标安全函数变为 `pub` 以供测试（例如 `scrub_credentials`、`SecurityPolicy::evaluate`）
- 在 `src/security/` 中添加 `#[cfg(test)] pub` 逃生口
- 改为在 `src/security/tests.rs` 中编写 crate 内单元测试

**5. `pub(crate)` 可见性阻止了关键子系统的集成测试**

`security` 和 `gateway` 模块使用 `pub(crate)` 可见性，阻止集成测试执行核心逻辑，如 `SecurityPolicy`、`GatewayRateLimiter` 和 `IdempotencyStore`。这迫使新的组件测试只能通过狭窄的公共 API 表面（配置结构体、一个签名函数、常量）进行测试。考虑关键安全类型是否应该暴露仅用于测试的公共接口，或者这些测试是否应该作为 crate 内单元测试。

### TODO：自动发布公告 —— Twitter/X 集成

**当前状态：** 发布仅在 GitHub 上发布。没有自动交叉发布到社交渠道。

**计划：**

- 添加 `.github/workflows/release-tweet.yml`，在 `release: [published]` 时触发
- 使用 `nearform-actions/github-action-notify-twitter`（OAuth 1.0a、v1.1 API）或带 OAuth 签名的直接 X API v2 `curl`
- 推文模板：发布标签、单行摘要、GitHub 发布链接
- 跳过预发布（`if: "!github.event.release.prerelease"`）

**所需密钥（设置 > 密钥 > Actions）：**

- `TWITTER_API_KEY`、`TWITTER_API_KEY_SECRET`
- `TWITTER_ACCESS_TOKEN`、`TWITTER_ACCESS_TOKEN_SECRET`

**注意事项：**

- 对照 [docs/contributing/actions-source-policy.md](../contributing/actions-source-policy.zh-CN.md) 审核 —— 将第三方操作固定到提交 SHA 或 vendor
- X 免费层级：每月 1,500 条推文（足够发布使用）
- 如果在推文中包含亮点，将发布正文截断为 280 字符
