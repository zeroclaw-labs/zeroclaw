# ZeroClaw 架构学习指南

## 📚 快速导航

本项目已创建完整的架构可视化图集，请打开以下文件查看:

1. **`architecture-index.html`** - 主索引页面 (从这里开始)
2. **`architecture-overview.html`** - 核心层次结构图
3. **`request-lifecycle.html`** - 请求生命周期流程图
4. **`extension-points.html`** - 关键扩展点详解
5. **`crates-dependencies.html`** - Crates依赖关系图

---

## 🎯 学习目标

### 初级阶段 (1-2周)
**目标**: 理解ZeroClaw的核心概念和基本架构

#### 必读文档
1. `docs/book/src/architecture/overview.md` - 整体架构介绍
2. `docs/book/src/foundations/fnd-001-microkernel.md` - 微内核设计理念
3. `docs/book/src/developing/extension-examples.md` - 扩展示例

#### 核心代码阅读
```bash
# 1. API Traits定义 (所有扩展的基础)
crates/zeroclaw-api/src/lib.rs
crates/zeroclaw-api/src/model_provider.rs
crates/zeroclaw-api/src/channel.rs
crates/zeroclaw-api/src/tool.rs

# 2. 配置Schema
crates/zeroclaw-config/src/schema.rs  # 只看关键部分
```

#### 实践任务
- [ ] 编译项目并运行daemon模式
- [ ] 修改配置文件,添加一个新的LLM provider
- [ ] 尝试使用CLI与Agent对话

---

### 中级阶段 (2-4周)
**目标**: 深入理解运行时机制和安全模型

#### 核心模块研读
```bash
# Agent循环 - 系统的心脏
crates/zeroclaw-runtime/src/agent/loop_.rs        # 主循环逻辑
crates/zeroclaw-runtime/src/agent/turn/mod.rs     # Turn引擎编排

# 安全策略 - 系统的守护者
crates/zeroclaw-runtime/src/security/policy.rs    # 安全策略核心
crates/zeroclaw-runtime/src/security/sandbox.rs   # 沙箱检测

# 记忆系统 - 系统的长期记忆
crates/zeroclaw-memory/src/backends/mod.rs        # Memory后端实现
```

#### 数据流追踪练习
选择一个简单的用户消息,追踪其完整生命周期:
1. Channel接收消息 (`crates/zeroclaw-channels/src/discord/mod.rs`)
2. 消息标准化和Attribution (`crates/zeroclaw-log/src/event.rs`)
3. Agent Loop处理 (`crates/zeroclaw-runtime/src/agent/loop_.rs`)
4. Provider调用和流式响应 (`crates/zeroclaw-providers/src/openai.rs`)
5. Tool执行 (如果需要) (`crates/zeroclaw-tools/src/shell/mod.rs`)
6. Memory持久化 (`crates/zeroclaw-memory/src/backends/markdown.rs`)

#### 实践任务
- [ ] 实现一个简单的自定义Tool (例如:天气查询工具)
- [ ] 为现有Channel添加一个新功能 (例如:Discord的embed支持)
- [ ] 调试并修复一个小bug

---

### 高级阶段 (1-2月)
**目标**: 掌握系统设计和扩展能力

#### 深度专题研究

##### 1. Plugin System (WASM运行时)
```bash
crates/zeroclaw-plugins/src/loader.rs      # WASM加载器
crates/zeroclaw-plugins/src/host.rs        # 宿主环境
crates/zeroclaw-plugins/src/plugin_trait.rs # Plugin trait定义
```

##### 2. Gateway服务器
```bash
crates/zeroclaw-gateway/src/server.rs      # Axum服务器设置
crates/zeroclaw-gateway/src/routes/*.rs    # REST API路由
crates/zeroclaw-gateway/src/websocket.rs   # WebSocket流式传输
```

##### 3. Hardware Abstraction Layer
```bash
crates/zeroclaw-hardware/src/discovery.rs  # USB设备发现
crates/zeroclaw-hardware/src/peripherals/*.rs # 外设实现
crates/aardvark-sys/src/lib.rs             # FFI绑定 (唯一允许unsafe的crate)
```

##### 4. Robot Kit
```bash
crates/robot-kit/src/drive/*.rs            # 驱动控制
crates/robot-kit/src/vision/*.rs           # 视觉处理
crates/robot-kit/src/safety/*.rs           # 安全系统
```

#### 架构设计原则研究
阅读所有RFC文档:
- `docs/book/src/rfcs/rfc-0001-single-source-of-truth.md`
- `docs/book/src/rfcs/rfc-0002-attribution-propagation.md`
- `docs/book/src/rfcs/rfc-0003-tool-receipts.md`
- `docs/book/src/rfcs/rfc-0004-estop.md`
- `docs/book/src/rfcs/rfc-0005-plugin-system.md`

#### 实践任务
- [ ] 开发一个完整的WASM插件
- [ ] 为Robot Kit添加一个新的传感器驱动
- [ ] 优化某个性能瓶颈 (使用cargo flamegraph分析)
- [ ] 提交一个有意义的PR到主仓库

---

## 🔍 关键概念解析

### 1. Microkernel Architecture (微内核架构)

ZeroClaw采用**trait-based微内核设计**,核心思想:
- **最小化内核**: `zeroclaw-api`只定义trait,不包含任何具体实现
- **插件化扩展**: 所有功能通过实现trait来添加
- **编译时强制模块化**: Rust的类型系统确保组件边界清晰

**优势**:
- ✅ 易于理解和维护
- ✅ 新功能不影响核心
- ✅ 测试隔离性好
- ✅ 支持多种编译profile优化

### 2. Attribution Propagation (归属传播)

每个组件都有唯一的身份标识(`Attributable` trait):
```rust
pub trait Attributable {
    fn role(&self) -> Role;      // Operator/System/Tool
    fn alias(&self) -> &str;     // 唯一标识符
}
```

**用途**:
- 审计日志中的责任追踪
- 安全策略的执行依据
- Tool Receipts的签名验证

### 3. Tool Receipts (工具收据)

每次工具执行都会生成加密签名的收据:
```toml
[tool_call.receipt]
id = "uuid"
timestamp = "ISO8601"
hmac = "HMAC-SHA256签名"
```

**安全特性**:
- 防止重放攻击
- 提供不可否认性
- 支持事后审计

### 4. Autonomy Levels (自主级别)

三级自主控制系统:
```rust
enum AutonomyLevel {
    ReadOnly,      // 只读操作
    Supervised,    // 需要批准
    Full,          // 完全自主
}
```

**风险评估矩阵**:
| 自主级别 | Low Risk | Medium Risk | High Risk | Critical Risk |
|----------|----------|-------------|-----------|---------------|
| ReadOnly | Deny     | Deny        | Deny      | Deny          |
| Supervised | Allow  | Approve     | Deny      | Deny          |
| Full     | Allow    | Allow       | Approve   | Deny          |

### 5. Single Source of Truth (单一事实来源)

**绝对规则**: 任何状态在系统中只能有一个权威来源。

**反面例子** (禁止):
```rust
// ❌ 错误: 重复存储allowed_users
struct ChannelHandle {
    allowed_users: Vec<String>,  // 缓存副本
    config_path: String,         // 实际配置在这里
}
```

**正确做法**:
```rust
// ✅ 正确: 通过闭包实时解析
struct ChannelHandle {
    resolve_allowed_users: Arc<dyn Fn() -> Vec<String>>,
}
```

---

## 🛠️ 开发工作流

### 1. 环境搭建
```bash
# 安装Rust工具链
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 克隆仓库
git clone https://github.com/zeroclaw/zeroclaw.git
cd zeroclaw

# 安装开发工具
cargo install cargo-watch
cargo install cargo-flamegraph
cargo install cargo-audit
```

### 2. 常用命令
```bash
# 格式化检查
cargo fmt --all -- --check

# Clippy检查
cargo clippy --all-targets -- -D warnings

# 运行测试
cargo test

# 完整CI检查
./dev/ci.sh all

# 开发模式运行
cargo run -- daemon --verbose

# 发布构建
cargo build --release --profile release-small
```

### 3. 调试技巧

#### 启用详细日志
```bash
RUST_LOG=debug cargo run -- daemon
# 或
export RUST_LOG=zeroclaw=debug,tokio=trace
```

#### 查看JSONL日志
```bash
tail -f ~/.local/share/zeroclaw/logs/zeroclaw.jsonl | jq .
```

#### 性能分析
```bash
# CPU profiling
cargo flamegraph --root --freq 4000 -- ./target/release/zeroclaw daemon

# 内存分析
cargo bench --bench memory_benchmark
```

#### 单步调试
```bash
# 使用lldb调试
lldb target/debug/zeroclaw
(lldb) break set -name run_tool_call_loop
(lldb) run daemon
```

---

## 📖 推荐学习路径

### 按角色分类

#### 如果你是AI/ML工程师
重点关注:
1. `crates/zeroclaw-providers/` - LLM集成
2. `crates/zeroclaw-runtime/src/agent/` - Agent循环
3. `docs/book/src/llm-integration/` - LLM集成指南

#### 如果你是系统程序员
重点关注:
1. `crates/zeroclaw-runtime/src/security/` - 安全模型
2. `crates/zeroclaw-spawn/` - 并发原语
3. `crates/aardvark-sys/` - FFI绑定

#### 如果你是Web开发者
重点关注:
1. `crates/zeroclaw-gateway/` - HTTP/WebSocket服务器
2. `apps/zerocode/` - TUI应用
3. `crates/zeroclaw-channels/src/webhook/` - Webhook处理

#### 如果你是嵌入式开发者
重点关注:
1. `crates/zeroclaw-hardware/` - 硬件抽象层
2. `crates/robot-kit/` - 机器人控制
3. `crates/aardvark-sys/` - I2C/SPI通信

---

## 🎓 进阶主题

### 1. WASM插件系统深度

**Plugin Host架构**:
```
┌─────────────────┐
│  Plugin Loader  │ ← 加载.wasm文件
├─────────────────┤
│  Wasmtime VM    │ ← JIT/AOT编译
├─────────────────┤
│  Host Functions │ ← 暴露给plugin的API
│  - log!         │
│  - alloc/dealloc│
│  - tool_invoke  │
└─────────────────┘
```

**编写Plugin**:
```rust
// plugin.rs
use zeroclaw_plugins::prelude::*;

#[plugin_export]
pub fn my_tool(args: PluginArgs) -> PluginResult {
    log_info!("Executing my tool");
    
    let input = args.get("input")?;
    let result = process(input);
    
    Ok(PluginResult {
        success: true,
        output: result,
    })
}
```

**编译Plugin**:
```bash
cargo build --target wasm32-wasi --release
cp target/wasm32-wasi/release/my_plugin.wasm ~/.config/zeroclaw/plugins/
```

### 2. Gateway安全模型

**认证流程**:
```
Client                    Gateway                    Backend
  │                         │                          │
  │───配对请求────────────>│                          │
  │                         │───生成配对码────────────>│
  │<──配对码显示───────────│                          │
  │                         │                          │
  │───输入配对码──────────>│                          │
  │                         │───验证配对码────────────>│
  │                         │<──JWT Token─────────────│
  │<──JWT Token + Cookie───│                          │
  │                         │                          │
  │───带Token的请求───────>│───验证签名──────────────>│
  │                         │<──授权结果──────────────│
  │<──响应─────────────────│                          │
```

**安全头设置**:
```rust
response
    .insert_header("X-Frame-Options", "DENY")
    .insert_header("X-Content-Type-Options", "nosniff")
    .insert_header("Strict-Transport-Security", "max-age=31536000");
```

### 3. Memory后端优化

**Markdown后端** (默认):
```rust
// 文件结构
~/.local/share/zeroclaw/memory/
├── conversations/
│   ├── 2024-01-15_assistant_abc123.md
│   └── 2024-01-16_assistant_def456.md
└── facts/
    ├── entity_foo.md
    └── concept_bar.md
```

**SQLite后端** (高性能):
```sql
-- Schema
CREATE TABLE conversations (
    id TEXT PRIMARY KEY,
    agent_alias TEXT NOT NULL,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    messages JSONB NOT NULL
);

CREATE INDEX idx_conversations_agent ON conversations(agent_alias);
CREATE INDEX idx_conversations_created ON conversations(created_at);
```

**向量搜索后端** (语义检索):
```rust
// 使用pgvector
impl VectorMemoryBackend {
    pub async fn search_similar(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<Fact>> {
        let sql = r#"
            SELECT content, embedding <-> $1 AS distance
            FROM facts
            ORDER BY distance
            LIMIT $2
        "#;
        
        self.pool.query(sql, &[&query_embedding, &(limit as i64)])
            .await?
            .iter()
            .map(|row| Fact::from_row(row))
            .collect()
    }
}
```

---

## 🚀 贡献指南

### 1. 找到合适的Issue
- 标记为`good first issue`的新手任务
- 标记为`help wanted`的求助任务
- 自己发现的bug或改进点

### 2. Fork和Branch
```bash
# Fork仓库
gh repo fork zeroclaw/zeroclaw --clone

# 创建feature branch
git checkout -b feature/my-awesome-feature

# 或者fix branch
git checkout -b fix/issue-1234
```

### 3. 开发和测试
```bash
# 保持分支更新
git fetch upstream
git rebase upstream/master

# 运行相关测试
cargo test --package zeroclaw-tools

# 格式化和clippy
cargo fmt
cargo clippy -- -D warnings
```

### 4. 提交PR
```bash
# Conventional Commit标题
git commit -m "feat(tools): add weather tool for OpenWeatherMap

- Implement WeatherTool with city-based lookup
- Add temperature unit conversion (Celsius/Fahrenheit)
- Include humidity and wind speed in response
- Closes #1234"

# Push并创建PR
git push origin feature/my-awesome-feature
gh pr create --fill
```

### 5. PR模板要求
确保PR描述包含:
- [x] 变更类型 (feat/fix/docs/chore)
- [x] 影响范围 (哪个crate)
- [x] 测试证据 (截图/日志)
- [x] 风险评估 (低/中/高)
- [ ] Breaking changes说明

---

## 📊 性能基准

### 二进制大小对比
| Profile | 大小 | 启动时间 | 适用场景 |
|---------|------|----------|----------|
| `dev` | ~50MB | 慢 | 开发调试 |
| `release` | ~15MB | 中等 | 生产部署 |
| `release-small` | ~8MB | 快 | 嵌入式设备 |
| `plugins-wasm-runtime-only` | ~5MB | 最快 | 最小化部署 |

### 内存使用
| 模式 | 空闲内存 | 峰值内存 |
|------|----------|----------|
| Daemon (无连接) | ~50MB | - |
| 单个Agent会话 | ~80MB | ~150MB |
| 多Agent并发 (4个) | ~200MB | ~400MB |
| 带WASM插件 | +20MB/插件 | +50MB/插件 |

### 延迟指标
| 操作 | P50 | P95 | P99 |
|------|-----|-----|-----|
| Tool调用 | 5ms | 15ms | 50ms |
| Memory读取 | 2ms | 8ms | 20ms |
| Provider API调用 | 200ms | 800ms | 2000ms |
| Channel消息发送 | 10ms | 50ms | 100ms |

---

## 🔮 未来方向

### Roadmap 2024 Q3-Q4
- [ ] **Plugin Marketplace**: WASM插件商店
- [ ] **Multi-Agent Orchestration**: Agent协作框架
- [ ] **Advanced RAG**: 检索增强生成优化
- [ ] **Hardware Support Expansion**: 更多开发板支持
- [ ] **Performance Optimization**: 启动时间和内存优化

### Research Topics
- **Federated Learning**: 分布式Agent训练
- **Formal Verification**: 安全属性的形式化证明
- **Quantum-Resistant Cryptography**: 抗量子加密
- **Neuromorphic Computing**: 类脑计算集成

---

## 📞 社区资源

### 官方渠道
- **GitHub**: https://github.com/zeroclaw/zeroclaw
- **Discord**: https://discord.gg/zeroclaw
- **Documentation**: https://zeroclaw.dev/docs

### 第三方资源
- **Awesome ZeroClaw**: 社区维护的资源列表
- **ZeroClaw Weekly**: 每周通讯
- **YouTube频道**: 教程和演示视频

### 联系方式
- **核心开发者**: @maintainers (GitHub)
- **邮件列表**: dev@zeroclaw.dev
- **Bug报告**: GitHub Issues

---

## 🎉 结语

ZeroClaw不仅仅是一个AI代理运行时,更是一个精心设计的软件工程范例。通过学习这个项目,你不仅能掌握AI Agent的开发技能,还能深入理解:

- **Rust高级特性**: Trait对象、异步编程、零成本抽象
- **系统设计**: 微内核架构、插件系统、安全模型
- **分布式系统**: 并发控制、消息传递、容错机制
- **密码学应用**: HMAC签名、AEAD加密、密钥管理

最重要的是,ZeroClaw展示了如何在保证安全性的前提下,构建一个既强大又灵活的AI系统。希望这份学习指南能帮助你踏上这段精彩的旅程!

**记住**: 
> "不要重复造轮子,但要理解轮子为什么这样设计。"

祝你学习愉快! 🚀
