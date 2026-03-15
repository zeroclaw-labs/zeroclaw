# MQTT Tethered Nodes — 树莓派 Master + ESP32 Node 架构

## TL;DR

> **Quick Summary**: 为 ZeroClaw 添加 MQTT 传输层支持，使 ESP32 节点可以通过 MQTT 连接到树莓派 master，复用现有 Node Discovery 系统（`/ws/nodes`），零改动核心代码。
> 
> **Deliverables**:
> - MQTT-to-WebSocket Bridge 独立服务（Rust）
> - ESP32 固件参考实现（Arduino/ESP-IDF + Berry Script）
> - Berry Script 脚本缓存系统
> - 配置文件和部署文档
> - 集成测试套件
> 
> **Estimated Effort**: Medium
> **Parallel Execution**: YES - 3 waves
> **Critical Path**: Schema → Bridge Core → Integration Tests

---

## Context

### Original Request
用户需求：规划 ZeroClaw 树莓派 master 作为 master 节点，ESP32 作为 node 节点以 tethered 执行 zeroclaw 的操作。通过 MQTT 协议互联，但 MQTT 协议要兼任已有的 serial socket 通讯，不更改已有架构，通过加一个兼容层来进行兼容，零改动已有 zeroclaw 代码。

### Interview Summary

**Key Discussions**:
- ESP32 角色：纯执行器 + 支持缓存脚本代码
- MQTT broker：树莓派本地运行 mosquitto
- 兼容层：Protocol 适配器模式
- 零侵入：通过独立 bridge 服务实现

**Research Findings**:
- 发现现有 Node Discovery 系统（`src/gateway/nodes.rs`）
- WebSocket-based 协议已完整定义（register/invoke/result）
- NodeRegistry 管理所有连接节点
- 已有 Bearer token 认证机制

**Architecture Decision**:
采用 MQTT-to-WebSocket Bridge 模式：
1. 独立 Rust 服务，订阅 MQTT topics
2. 转换 MQTT 消息为 WebSocket 协议
3. 连接到现有 `/ws/nodes` endpoint
4. 零改动 ZeroClaw 核心代码

### Metis Review

**Identified Gaps** (addressed):
- MQTT topic 结构未定义 → 已定义标准 topic 结构
- 消息格式兼容性未验证 → 已确认使用相同 JSON schema
- 认证策略缺失 → 复用现有 Bearer token 机制
- 错误恢复策略缺失 → 添加自动重连和心跳机制
- 测试策略缺失 → 添加完整 TDD 测试计划
- ESP32 固件范围不明确 → 明确为参考实现（包含 Berry Script）
- 部署模型未指定 → 定义为独立 systemd 服务
- 脚本缓存机制 → 使用 Berry Script（40KB Flash + 4KB RAM）

---

## Work Objectives

### Core Objective
为 ZeroClaw 添加 MQTT 传输层支持，使 ESP32 等嵌入式设备可以通过 MQTT 协议连接到 ZeroClaw master 节点，复用现有 Node Discovery 系统，实现零侵入集成。

### Concrete Deliverables
- `zeroclaw-bridge` 独立二进制（Rust）
- MQTT topic 结构定义文档
- 消息 schema 定义（JSON）
- ESP32 固件参考实现（Arduino）
- 配置文件模板（`bridge.toml`）
- Systemd service 文件
- 集成测试套件
- 部署和运维文档

### Definition of Done
- [ ] `cargo build --release` 成功编译 bridge
- [ ] `cargo test` 所有测试通过
- [ ] `mosquitto_pub` + `curl` 验证端到端流程
- [ ] ESP32 固件可以注册并执行命令
- [ ] Bridge 作为 systemd 服务运行
- [ ] 文档完整（部署、配置、故障排查）

### Must Have
- MQTT-to-WebSocket 双向消息转发
- 节点注册和能力发现
- 命令调用和结果返回
- 自动重连机制（MQTT + WebSocket）
- Bearer token 认证
- 心跳和健康检查
- 配置文件加载
- 结构化日志

### Must NOT Have (Guardrails)
- ❌ 修改 ZeroClaw 核心代码（`src/gateway/nodes.rs` 等）
- ❌ 实现完整通用脚本运行时（支持任意代码执行）
- ❌ 在 bridge 中复制 NodeRegistry 逻辑
- ❌ 创建新的认证系统（复用现有 Bearer token）
- ❌ 支持多 broker 或 HA 部署（V1 范围外）
- ❌ 实现消息持久化或队列（保持 stateless）
- ❌ 添加 Web UI 或管理界面

### Must Have (Updated - 包含脚本缓存)
- MQTT-to-WebSocket 双向消息转发
- 节点注册和能力发现
- 命令调用和结果返回
- 自动重连机制（MQTT + WebSocket）
- Bearer token 认证
- 心跳和健康检查
- 配置文件加载
- 结构化日志
- **Berry Script 脚本缓存和执行**（ESP32 端）

---

## Verification Strategy (MANDATORY)

> **ZERO HUMAN INTERVENTION** — ALL verification is agent-executed. No exceptions.

### Test Decision
- **Infrastructure exists**: YES (Rust + cargo test + ESP32 QEMU)
- **Automated tests**: TDD (RED-GREEN-REFACTOR)
- **Framework**: Rust `#[test]` + `tokio::test` + ESP32 QEMU 模拟器
- **TDD Flow**: 每个任务先写失败测试 → 最小实现 → 重构
- **ESP32 Testing**: 使用 QEMU ESP32 模拟器进行固件测试（无需物理硬件）

### QA Policy
每个任务包含 agent-executed QA scenarios。
Evidence 保存到 `.sisyphus/evidence/task-{N}-{scenario-slug}.{ext}`。

- **Bridge 功能**: 使用 `cargo test` + `mosquitto_pub/sub` + `curl`
- **ESP32 固件**: 使用 **ESP32 QEMU 模拟器** + `platformio test`（无需物理硬件）
- **集成测试**: 使用 `docker-compose` 启动完整环境（包括 QEMU ESP32）

---

## Execution Strategy

### Parallel Execution Waves

> 最大化并行执行。每个 wave 完成后才开始下一个。

```
Wave 1 (Foundation — 可立即开始，5 tasks):
├── Task 1: MQTT topic 结构和消息 schema 定义 [writing]
├── Task 2: Bridge 项目脚手架和配置加载 [quick]
├── Task 3: MQTT client 封装（rumqttc） [quick]
├── Task 4: WebSocket client 封装（tokio-tungstenite） [quick]
└── Task 5: ESP32 固件项目脚手架（Arduino） [quick]

Wave 2 (Core Logic — 依赖 Wave 1，4 tasks):
├── Task 6: 消息转换逻辑（MQTT ↔ WebSocket） [unspecified-high]
├── Task 7: Bridge 核心事件循环 [deep]
├── Task 8: 认证和 token 传递 [unspecified-high]
└── Task 9: ESP32 命令执行器（GPIO/ADC） [unspecified-high]

Wave 3 (Integration — 依赖 Wave 2，3 tasks):
├── Task 10: 自动重连和错误恢复 [deep]
├── Task 11: 心跳和健康检查 [unspecified-high]
└── Task 12: ESP32 能力注册和生命周期 [unspecified-high]

Wave 4 (Berry Script — 依赖 Wave 3，3 tasks):
├── Task 13: Berry Script VM 集成（ESP32） [deep]
├── Task 14: 脚本缓存系统（SPIFFS + script_cache/execute） [unspecified-high]
└── Task 15: Berry Script QEMU 测试 [unspecified-high]

Wave 5 (Testing & Deployment — 依赖 Wave 4，4 tasks):
├── Task 16: 集成测试套件（docker-compose + QEMU） [deep]
├── Task 17: Systemd service 和部署脚本 [quick]
├── Task 18: 运维文档（部署、配置、故障排查） [writing]
└── Task 19: ESP32 固件示例和文档（含 Berry） [writing]

Wave FINAL (Verification — 独立审查，4 parallel):
├── Task F1: Plan compliance audit (oracle)
├── Task F2: Code quality review (unspecified-high)
├── Task F3: Real manual QA (unspecified-high)
└── Task F4: Scope fidelity check (deep)

Critical Path: Task 1 → Task 6 → Task 7 → Task 10 → Task 13 → Task 16 → F1-F4
Parallel Speedup: ~65% faster than sequential
Max Concurrent: 5 (Wave 1)
```

### Dependency Matrix

- **Wave 1 (1-5)**: — → 6-9, 1
- **Task 6**: 1, 3, 4 → 7, 2
- **Task 7**: 2, 6 → 8, 10, 11, 3
- **Task 8**: 7 → 10, 3
- **Task 9**: 5 → 12, 2
- **Task 10**: 7, 8 → 13, 3
- **Task 11**: 7 → 13, 3
- **Task 12**: 9 → 13, 3
- **Task 13**: 10, 11, 12 → 14, 15, 16, 4
- **Task 14-16**: 13 → F1-F4, 4

### Agent Dispatch Summary

- **Wave 1**: 5 tasks → T1 `writing`, T2-4 `quick`, T5 `quick`
- **Wave 2**: 4 tasks → T6 `unspecified-high`, T7 `deep`, T8-9 `unspecified-high`
- **Wave 3**: 3 tasks → T10 `deep`, T11-12 `unspecified-high`
- **Wave 4**: 3 tasks → T13 `deep`, T14-15 `unspecified-high`
- **Wave 5**: 4 tasks → T16 `deep`, T17 `quick`, T18-19 `writing`
- **FINAL**: 4 tasks → F1 `oracle`, F2-4 `unspecified-high`/`deep`

---

## TODOs

- [x] 1. MQTT Topic 结构和消息 Schema 定义

  **What to do**:
  - 定义 MQTT topic 结构（register/invoke/result/heartbeat）
  - 编写 JSON schema 文档（与 WebSocket 协议对齐）
  - 创建 `docs/architecture/mqtt-bridge-protocol.md`

  **Must NOT do**:
  - 不要创建与 WebSocket 不兼容的消息格式
  - 不要添加 ZeroClaw 核心不支持的字段

  **Recommended Agent Profile**:
  - **Category**: `writing`
    - Reason: 主要是文档编写和协议设计
  - **Skills**: []
  - **Skills Evaluated but Omitted**: 无

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 2, 3, 4, 5)
  - **Blocks**: Tasks 6, 7, 8
  - **Blocked By**: None (can start immediately)

  **References**:
  - `src/gateway/nodes.rs:149-183` - 现有 WebSocket 消息格式（NodeMessage/GatewayMessage）
  - `src/gateway/nodes.rs:38-52` - NodeCapability 结构定义
  - `src/gateway/nodes.rs:74-78` - NodeInvocationResult 结构

  **Acceptance Criteria**:
  - [ ] 文档创建：`docs/architecture/mqtt-bridge-protocol.md` 存在
  - [ ] Topic 结构定义：包含 4 个 topic（register/invoke/result/heartbeat）
  - [ ] 消息示例：每个 topic 至少 1 个完整 JSON 示例
  - [ ] Schema 对齐：与 `src/gateway/nodes.rs` 中的结构一致

  **QA Scenarios**:
  ```
  Scenario: 验证文档完整性
    Tool: Bash (grep)
    Steps:
      1. grep -E "zeroclaw/nodes/.*/register" docs/architecture/mqtt-bridge-protocol.md
      2. grep -E "zeroclaw/nodes/.*/invoke" docs/architecture/mqtt-bridge-protocol.md
      3. grep -E "zeroclaw/nodes/.*/result" docs/architecture/mqtt-bridge-protocol.md
      4. grep -E "zeroclaw/nodes/.*/heartbeat" docs/architecture/mqtt-bridge-protocol.md
    Expected Result: 每个 grep 返回至少 1 行匹配
    Evidence: .sisyphus/evidence/task-1-doc-completeness.txt

  Scenario: 验证 JSON 示例有效性
    Tool: Bash (jq)
    Steps:
      1. 从文档中提取 JSON 代码块
      2. echo '{"type":"register","node_id":"esp32-001","capabilities":[]}' | jq .
      3. 验证所有示例都是有效 JSON
    Expected Result: jq 解析成功，无语法错误
    Evidence: .sisyphus/evidence/task-1-json-validation.txt
  ```

  **Commit**: YES
  - Message: `docs(bridge): add MQTT bridge protocol specification`
  - Files: `docs/architecture/mqtt-bridge-protocol.md`

- [x] 2. Bridge 项目脚手架和配置加载

  **What to do**:
  - 创建 `crates/zeroclaw-bridge/` 子项目
  - 添加 `Cargo.toml` 依赖（rumqttc, tokio-tungstenite, serde, tracing）
  - 实现配置文件加载（`bridge.toml`）
  - 创建 `src/config.rs` 和 `src/main.rs` 骨架

  **Must NOT do**:
  - 不要在 ZeroClaw 主项目中添加代码
  - 不要实现业务逻辑（仅脚手架）

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: 标准 Rust 项目初始化
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1
  - **Blocks**: Tasks 6, 7
  - **Blocked By**: None

  **References**:
  - `Cargo.toml` - 主项目 workspace 配置
  - `src/config/schema.rs` - ZeroClaw 配置加载模式

  **Acceptance Criteria**:
  - [ ] `cargo build -p zeroclaw-bridge` 编译成功
  - [ ] `crates/zeroclaw-bridge/bridge.toml.example` 存在
  - [ ] 配置结构包含：mqtt_broker_url, websocket_url, auth_token

  **QA Scenarios**:
  ```
  Scenario: 编译验证
    Tool: Bash (cargo)
    Steps:
      1. cd /home/whereslow/projects/zeroclaw-micro
      2. cargo build -p zeroclaw-bridge
    Expected Result: exit code 0, 无编译错误
    Evidence: .sisyphus/evidence/task-2-build.txt

  Scenario: 配置文件加载测试
    Tool: Bash (cargo test)
    Steps:
      1. cargo test -p zeroclaw-bridge config::tests::load_example_config
    Expected Result: 测试通过
    Evidence: .sisyphus/evidence/task-2-config-test.txt
  ```

  **Commit**: YES
  - Message: `feat(bridge): add project scaffold and config loading`
  - Files: `crates/zeroclaw-bridge/**`

- [x] 3. MQTT Client 封装（rumqttc）

  **What to do**:
  - 创建 `src/mqtt_client.rs`
  - 封装 rumqttc 连接、订阅、发布
  - 实现自动重连逻辑
  - 添加单元测试（使用 mock）

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1
  - **Blocks**: Task 6
  - **Blocked By**: None

  **References**:
  - rumqttc docs: https://docs.rs/rumqttc/latest/rumqttc/

  **Acceptance Criteria**:
  - [ ] `cargo test -p zeroclaw-bridge mqtt_client::tests` 通过
  - [ ] 支持 connect/subscribe/publish 方法

  **QA Scenarios**:
  ```
  Scenario: MQTT 连接测试
    Tool: Bash (cargo test)
    Steps:
      1. cargo test -p zeroclaw-bridge mqtt_client::tests::connect_success
    Expected Result: 测试通过
    Evidence: .sisyphus/evidence/task-3-mqtt-test.txt
  ```

  **Commit**: YES
  - Message: `feat(bridge): add MQTT client wrapper`
  - Files: `crates/zeroclaw-bridge/src/mqtt_client.rs`

- [x] 4. WebSocket Client 封装（tokio-tungstenite）

  **What to do**:
  - 创建 `src/ws_client.rs`
  - 封装 WebSocket 连接、发送、接收
  - 实现自动重连逻辑
  - 添加单元测试

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1
  - **Blocks**: Task 6
  - **Blocked By**: None

  **References**:
  - tokio-tungstenite docs: https://docs.rs/tokio-tungstenite/

  **Acceptance Criteria**:
  - [ ] `cargo test -p zeroclaw-bridge ws_client::tests` 通过
  - [ ] 支持 connect/send/receive 方法

  **QA Scenarios**:
  ```
  Scenario: WebSocket 连接测试
    Tool: Bash (cargo test)
    Steps:
      1. cargo test -p zeroclaw-bridge ws_client::tests::connect_success
    Expected Result: 测试通过
    Evidence: .sisyphus/evidence/task-4-ws-test.txt
  ```

  **Commit**: YES
  - Message: `feat(bridge): add WebSocket client wrapper`
  - Files: `crates/zeroclaw-bridge/src/ws_client.rs`

- [x] 5. ESP32 固件项目脚手架（Arduino + QEMU）

  **What to do**:
  - 创建 `firmware/esp32-node/` 目录
  - 添加 `platformio.ini` 配置（包含 QEMU 测试环境）
  - 创建基础 `main.cpp` 骨架（WiFi + MQTT client）
  - 配置 ESP32 QEMU 模拟器环境
  - 添加 QEMU 测试脚本
  - 添加 README 说明（包含 QEMU 使用）

  **Must NOT do**:
  - 不要依赖物理硬件进行测试
  - 不要跳过 QEMU 配置

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1
  - **Blocks**: Task 9, 12
  - **Blocked By**: None

  **References**:
  - PlatformIO ESP32 docs: https://docs.platformio.org/en/latest/boards/espressif32/
  - ESP32 QEMU: https://github.com/espressif/qemu

  **Acceptance Criteria**:
  - [ ] `platformio run -d firmware/esp32-node` 编译成功
  - [ ] 包含 WiFi 和 MQTT 库依赖
  - [ ] QEMU 测试环境配置完成
  - [ ] `./scripts/test-qemu.sh` 可以在 QEMU 中运行固件

  **QA Scenarios**:
  ```
  Scenario: 固件编译验证
    Tool: Bash (platformio)
    Steps:
      1. cd firmware/esp32-node
      2. platformio run
    Expected Result: exit code 0, 编译成功
    Evidence: .sisyphus/evidence/task-5-firmware-build.txt

  Scenario: QEMU 模拟器测试
    Tool: Bash (qemu + test script)
    Steps:
      1. cd firmware/esp32-node
      2. ./scripts/test-qemu.sh
      3. 验证固件在 QEMU 中启动
    Expected Result: QEMU 启动成功，固件运行无错误
    Evidence: .sisyphus/evidence/task-5-qemu-test.txt
  ```

  **Commit**: YES
  - Message: `feat(firmware): add ESP32 node firmware scaffold with QEMU support`
  - Files: `firmware/esp32-node/**`

- [x] 6. 消息转换逻辑（MQTT ↔ WebSocket）

  **What to do**:
  - 创建 `src/transform.rs`
  - 实现 MQTT → WebSocket 消息转换
  - 实现 WebSocket → MQTT 消息转换
  - 添加完整单元测试（TDD）

  **Must NOT do**:
  - 不要添加状态管理（保持 stateless）
  - 不要修改消息内容（纯转换）

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 2
  - **Blocks**: Task 7
  - **Blocked By**: Tasks 1, 3, 4

  **References**:
  - `docs/architecture/mqtt-bridge-protocol.md` - MQTT 消息格式
  - `src/gateway/nodes.rs:149-183` - WebSocket 消息格式

  **Acceptance Criteria**:
  - [ ] `cargo test -p zeroclaw-bridge transform::tests` 通过（至少 6 个测试）
  - [ ] 支持 register/invoke/result 三种消息类型转换

  **QA Scenarios**:
  ```
  Scenario: Register 消息转换
    Tool: Bash (cargo test)
    Steps:
      1. cargo test -p zeroclaw-bridge transform::tests::mqtt_to_ws_register
    Expected Result: 测试通过，MQTT payload 正确转换为 WebSocket JSON
    Evidence: .sisyphus/evidence/task-6-transform-register.txt

  Scenario: Invoke 消息转换
    Tool: Bash (cargo test)
    Steps:
      1. cargo test -p zeroclaw-bridge transform::tests::ws_to_mqtt_invoke
    Expected Result: 测试通过，WebSocket JSON 正确转换为 MQTT payload
    Evidence: .sisyphus/evidence/task-6-transform-invoke.txt
  ```

  **Commit**: YES
  - Message: `feat(bridge): add message transformation logic`
  - Files: `crates/zeroclaw-bridge/src/transform.rs`

- [x] 7. Bridge 核心事件循环

  **What to do**:
  - 创建 `src/bridge.rs`
  - 实现双向消息转发事件循环
  - 集成 MQTT client、WebSocket client、transform 模块
  - 使用 tokio::select! 处理并发事件

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 2
  - **Blocks**: Tasks 8, 10, 11
  - **Blocked By**: Tasks 2, 6

  **References**:
  - `src/gateway/nodes.rs:281-390` - WebSocket 事件处理模式

  **Acceptance Criteria**:
  - [ ] `cargo build -p zeroclaw-bridge` 编译成功
  - [ ] 事件循环可以同时处理 MQTT 和 WebSocket 消息

  **QA Scenarios**:
  ```
  Scenario: 事件循环启动
    Tool: Bash (cargo run)
    Steps:
      1. cargo run -p zeroclaw-bridge &
      2. sleep 2
      3. ps aux | grep zeroclaw-bridge
    Expected Result: 进程运行中
    Evidence: .sisyphus/evidence/task-7-bridge-running.txt
  ```

  **Commit**: YES
  - Message: `feat(bridge): add core event loop`
  - Files: `crates/zeroclaw-bridge/src/bridge.rs`

- [x] 8. 认证和 Token 传递

  **What to do**:
  - 在 WebSocket 连接时添加 Bearer token
  - 从配置文件读取 auth_token
  - 实现 token 验证逻辑

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 2
  - **Blocks**: Task 10
  - **Blocked By**: Task 7

  **References**:
  - `src/gateway/nodes.rs:191-230` - Token 提取逻辑

  **Acceptance Criteria**:
  - [ ] WebSocket 连接包含 Authorization header
  - [ ] 配置文件支持 auth_token 字段

  **QA Scenarios**:
  ```
  Scenario: Token 传递验证
    Tool: Bash (tcpdump + grep)
    Steps:
      1. 启动 bridge
      2. tcpdump -i lo -A | grep "Authorization: Bearer"
    Expected Result: 捕获到 Bearer token
    Evidence: .sisyphus/evidence/task-8-auth-token.txt
  ```

  **Commit**: YES
  - Message: `feat(bridge): add authentication and token passing`
  - Files: `crates/zeroclaw-bridge/src/bridge.rs`, `src/config.rs`

- [x] 9. ESP32 命令执行器（GPIO/ADC）+ QEMU 测试

  **What to do**:
  - 实现 GPIO 读写命令处理
  - 实现 ADC 读取命令处理
  - 添加命令白名单验证
  - 实现结果返回逻辑
  - 编写 QEMU 单元测试（模拟 GPIO/ADC）

  **Must NOT do**:
  - 不要依赖物理硬件进行测试
  - 不要跳过 QEMU 测试

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2
  - **Blocks**: Task 12
  - **Blocked By**: Task 5

  **References**:
  - `src/peripherals/serial.rs:191-275` - GPIO tool 实现参考
  - ESP32 QEMU GPIO 模拟文档

  **Acceptance Criteria**:
  - [ ] 支持 gpio_read/gpio_write/adc_read 命令
  - [ ] 命令白名单可配置
  - [ ] QEMU 单元测试通过

  **QA Scenarios**:
  ```
  Scenario: QEMU GPIO 写入测试
    Tool: Bash (qemu + test script)
    Steps:
      1. cd firmware/esp32-node
      2. ./scripts/test-qemu-gpio.sh write 2 1
      3. 验证 QEMU 日志显示 GPIO 2 设置为 HIGH
    Expected Result: 测试脚本 exit code 0
    Evidence: .sisyphus/evidence/task-9-qemu-gpio-write.txt

  Scenario: QEMU ADC 读取测试
    Tool: Bash (qemu + test script)
    Steps:
      1. cd firmware/esp32-node
      2. ./scripts/test-qemu-adc.sh read 34
      3. 验证返回模拟 ADC 值
    Expected Result: 返回值在 0-4095 范围内
    Evidence: .sisyphus/evidence/task-9-qemu-adc-read.txt
  ```

  **Commit**: YES
  - Message: `feat(firmware): add command executor for GPIO/ADC with QEMU tests`
  - Files: `firmware/esp32-node/src/executor.cpp`, `firmware/esp32-node/scripts/test-qemu-*.sh`

- [x] 10. 自动重连和错误恢复

  **What to do**:
  - 实现 MQTT 断线重连（指数退避）
  - 实现 WebSocket 断线重连
  - 添加错误日志和监控
  - 实现优雅关闭

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 3
  - **Blocks**: Task 13
  - **Blocked By**: Tasks 7, 8

  **References**:
  - rumqttc reconnection examples

  **Acceptance Criteria**:
  - [ ] MQTT 断线后 5 秒内自动重连
  - [ ] WebSocket 断线后 5 秒内自动重连

  **QA Scenarios**:
  ```
  Scenario: MQTT 重连测试
    Tool: Bash (systemctl + mosquitto_sub)
    Steps:
      1. 启动 bridge
      2. sudo systemctl stop mosquitto
      3. sleep 2
      4. sudo systemctl start mosquitto
      5. 检查 bridge 日志是否显示重连成功
    Expected Result: 日志包含 "MQTT reconnected"
    Evidence: .sisyphus/evidence/task-10-mqtt-reconnect.txt
  ```

  **Commit**: YES
  - Message: `feat(bridge): add auto-reconnection and error recovery`
  - Files: `crates/zeroclaw-bridge/src/bridge.rs`

- [x] 11. 心跳和健康检查

  **What to do**:
  - ESP32 每 30 秒发送心跳到 MQTT
  - Bridge 转发心跳到 WebSocket
  - 实现节点超时检测（90 秒无心跳则标记离线）

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3
  - **Blocks**: Task 13
  - **Blocked By**: Task 7

  **References**:
  - `src/gateway/nodes.rs:281-390` - 节点生命周期管理

  **Acceptance Criteria**:
  - [ ] ESP32 每 30 秒发送心跳
  - [ ] Bridge 日志显示心跳接收

  **QA Scenarios**:
  ```
  Scenario: 心跳发送验证
    Tool: Bash (mosquitto_sub)
    Steps:
      1. mosquitto_sub -t "zeroclaw/nodes/+/heartbeat" -v
      2. 等待 35 秒
    Expected Result: 至少收到 1 条心跳消息
    Evidence: .sisyphus/evidence/task-11-heartbeat.txt
  ```

  **Commit**: YES
  - Message: `feat(bridge): add heartbeat and health check`
  - Files: `crates/zeroclaw-bridge/src/bridge.rs`, `firmware/esp32-node/src/main.cpp`

- [x] 12. ESP32 能力注册和生命周期

  **What to do**:
  - ESP32 启动时发送 register 消息
  - 包含能力列表（gpio_read/gpio_write/adc_read）
  - 实现优雅关闭（发送 unregister）

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3
  - **Blocks**: Task 13
  - **Blocked By**: Task 9

  **References**:
  - `docs/architecture/mqtt-bridge-protocol.md` - Register 消息格式

  **Acceptance Criteria**:
  - [ ] ESP32 启动后 5 秒内发送 register
  - [ ] 能力列表包含至少 3 个命令

  **QA Scenarios**:
  ```
  Scenario: 注册消息验证
    Tool: Bash (mosquitto_sub + jq)
    Steps:
      1. mosquitto_sub -t "zeroclaw/nodes/+/register" -C 1 > /tmp/register.json
      2. jq '.capabilities | length' /tmp/register.json
    Expected Result: 输出 >= 3
    Evidence: .sisyphus/evidence/task-12-register.txt
  ```

  **Commit**: YES
  - Message: `feat(firmware): add capability registration and lifecycle`
  - Files: `firmware/esp32-node/src/main.cpp`

- [x] 13. Berry Script VM 集成（ESP32）

  **What to do**:
  - 集成 Berry Script VM 到 ESP32 固件
  - 添加 Berry 依赖到 platformio.ini
  - 暴露 GPIO/ADC/I2C 函数给 Berry
  - 实现 Berry 脚本执行接口
  - 添加 QEMU 单元测试

  **Must NOT do**:
  - 不要实现完整通用解释器（仅支持预定义 API）
  - 不要允许任意文件系统访问

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4
  - **Blocks**: Task 14, 16
  - **Blocked By**: Task 12

  **References**:
  - Berry Script docs: https://berry-lang.github.io/
  - Tasmota Berry integration: https://tasmota.github.io/docs/Berry/

  **Acceptance Criteria**:
  - [ ] Berry VM 初始化成功
  - [ ] 可以从 C++ 调用 Berry 脚本
  - [ ] GPIO/ADC 函数可从 Berry 访问
  - [ ] QEMU 测试通过

  **QA Scenarios**:
  ```
  Scenario: Berry VM 初始化测试
    Tool: Bash (qemu + test script)
    Steps:
      1. cd firmware/esp32-node
      2. ./scripts/test-qemu-berry-init.sh
      3. 验证 Berry VM 启动无错误
    Expected Result: 日志显示 "Berry VM initialized"
    Evidence: .sisyphus/evidence/task-13-berry-init.txt

  Scenario: Berry GPIO 调用测试
    Tool: Bash (qemu + berry script)
    Steps:
      1. echo 'import gpio; gpio.write(2, 1)' > /tmp/test.be
      2. ./scripts/test-qemu-berry-exec.sh /tmp/test.be
      3. 验证 GPIO 2 设置为 HIGH
    Expected Result: QEMU 日志显示 GPIO 操作成功
    Evidence: .sisyphus/evidence/task-13-berry-gpio.txt
  ```

  **Commit**: YES
  - Message: `feat(firmware): integrate Berry Script VM`
  - Files: `firmware/esp32-node/src/berry_vm.cpp`, `platformio.ini`

- [ ] 14. 脚本缓存系统（SPIFFS + script_cache/execute）

  **What to do**:
  - 实现 SPIFFS 文件系统初始化
  - 添加 script_cache 命令（保存脚本到 flash）
  - 添加 script_execute 命令（执行缓存脚本）
  - 添加 script_list/script_delete 命令
  - 实现脚本 ID 管理

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4
  - **Blocks**: Task 16
  - **Blocked By**: Task 13

  **References**:
  - ESP32 SPIFFS docs

  **Acceptance Criteria**:
  - [ ] 支持 script_cache/execute/list/delete 命令
  - [ ] 脚本持久化到 SPIFFS
  - [ ] QEMU 测试通过

  **QA Scenarios**:
  ```
  Scenario: 脚本缓存测试
    Tool: Bash (mosquitto_pub + qemu)
    Steps:
      1. mosquitto_pub -t "zeroclaw/nodes/esp32-test/invoke" -m '{"call_id":"c1","capability":"script_cache","args":{"script_id":"blink","code":"import gpio\ngpio.write(2,1)"}}'
      2. 验证 SPIFFS 中存在 blink.be 文件
    Expected Result: 返回 success
    Evidence: .sisyphus/evidence/task-14-script-cache.txt

  Scenario: 脚本执行测试
    Tool: Bash (mosquitto_pub + qemu)
    Steps:
      1. mosquitto_pub -t "zeroclaw/nodes/esp32-test/invoke" -m '{"call_id":"c2","capability":"script_execute","args":{"script_id":"blink"}}'
      2. 验证脚本执行成功
    Expected Result: GPIO 2 设置为 HIGH
    Evidence: .sisyphus/evidence/task-14-script-execute.txt
  ```

  **Commit**: YES
  - Message: `feat(firmware): add Berry script caching system`
  - Files: `firmware/esp32-node/src/script_cache.cpp`

- [ ] 15. Berry Script QEMU 测试套件

  **What to do**:
  - 编写完整 Berry 脚本测试用例
  - 测试 GPIO/ADC/I2C Berry API
  - 测试脚本缓存生命周期
  - 测试错误处理

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4
  - **Blocks**: Task 16
  - **Blocked By**: Task 13

  **Acceptance Criteria**:
  - [ ] 至少 10 个 Berry 测试用例
  - [ ] 覆盖所有暴露的 API

  **QA Scenarios**:
  ```
  Scenario: Berry 测试套件执行
    Tool: Bash (qemu + test runner)
    Steps:
      1. cd firmware/esp32-node
      2. ./scripts/run-berry-tests.sh
    Expected Result: 所有测试通过
    Evidence: .sisyphus/evidence/task-15-berry-tests.txt
  ```

  **Commit**: YES
  - Message: `test(firmware): add Berry Script test suite`
  - Files: `firmware/esp32-node/tests/berry/**`

---

  **What to do**:
  - 创建 `tests/integration/docker-compose.yml`
  - 包含 mosquitto + zeroclaw-gateway + zeroclaw-bridge + ESP32 QEMU 容器
  - 编写端到端测试脚本（bash + curl + mosquitto_pub）
  - 验证完整流程：ESP32 QEMU 注册 → Bridge 转发 → Gateway 调用 → 结果返回

  **Must NOT do**:
  - 不要依赖物理 ESP32 硬件
  - 不要跳过 QEMU 集成测试

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 4
  - **Blocks**: Tasks 14, 15, 16
  - **Blocked By**: Tasks 10, 11, 12

  **References**:
  - Docker Compose docs
  - ESP32 QEMU Docker image

  **Acceptance Criteria**:
  - [ ] `docker-compose up` 启动所有服务（包括 QEMU ESP32）
  - [ ] 测试脚本验证端到端流程
  - [ ] QEMU ESP32 成功注册到 Gateway

  **QA Scenarios**:
  ```
  Scenario: 端到端集成测试（含 QEMU）
    Tool: Bash (docker-compose + test script)
    Steps:
      1. cd tests/integration
      2. docker-compose up -d
      3. sleep 10  # 等待 QEMU ESP32 启动
      4. ./test-e2e-qemu.sh
      5. docker-compose down
    Expected Result: test-e2e-qemu.sh exit code 0, 所有测试通过
    Evidence: .sisyphus/evidence/task-13-e2e-qemu-test.txt

  Scenario: QEMU ESP32 注册验证
    Tool: Bash (curl + jq)
    Steps:
      1. docker-compose up -d
      2. sleep 10
      3. curl http://localhost:8080/api/nodes | jq '.nodes[] | select(.node_id | startswith("esp32-qemu"))'
    Expected Result: 返回 QEMU ESP32 节点信息
    Evidence: .sisyphus/evidence/task-13-qemu-registration.txt
  ```

  **Commit**: YES
  - Message: `test(firmware): add Berry Script test suite`
  - Files: `firmware/esp32-node/tests/berry/**`

- [ ] 16. 集成测试套件（docker-compose + QEMU + Berry）

  **What to do**:
  - 创建 `tests/integration/docker-compose.yml`
  - 包含 mosquitto + zeroclaw-gateway + zeroclaw-bridge + ESP32 QEMU 容器
  - 编写端到端测试脚本（bash + curl + mosquitto_pub）
  - **测试 Berry 脚本缓存和执行流程**
  - 验证完整流程：ESP32 QEMU 注册 → Bridge 转发 → Gateway 调用 → Berry 执行 → 结果返回

  **Must NOT do**:
  - 不要依赖物理 ESP32 硬件
  - 不要跳过 Berry 脚本测试

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 5
  - **Blocks**: Tasks 17, 18, 19
  - **Blocked By**: Tasks 13, 14, 15

  **References**:
  - Docker Compose docs
  - ESP32 QEMU Docker image

  **Acceptance Criteria**:
  - [ ] `docker-compose up` 启动所有服务（包括 QEMU ESP32）
  - [ ] 测试脚本验证端到端流程
  - [ ] QEMU ESP32 成功注册到 Gateway
  - [ ] **Berry 脚本缓存和执行测试通过**

  **QA Scenarios**:
  ```
  Scenario: 端到端 Berry 脚本测试
    Tool: Bash (docker-compose + test script)
    Steps:
      1. cd tests/integration
      2. docker-compose up -d
      3. sleep 10
      4. ./test-e2e-berry.sh
      5. docker-compose down
    Expected Result: Berry 脚本缓存、执行、结果返回全流程成功
    Evidence: .sisyphus/evidence/task-16-e2e-berry.txt
  ```

  **Commit**: YES
  - Message: `test(bridge): add integration test suite with Berry Script support`
  - Files: `tests/integration/**`

- [ ] 17. Systemd Service 和部署脚本

  **What to do**:
  - 创建 `zeroclaw-bridge.service` systemd 文件
  - 编写安装脚本 `install-bridge.sh`
  - 添加配置文件模板

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 5
  - **Blocks**: F1-F4
  - **Blocked By**: Task 16

  **Acceptance Criteria**:
  - [ ] `sudo systemctl start zeroclaw-bridge` 启动成功
  - [ ] `sudo systemctl status zeroclaw-bridge` 显示 active

  **QA Scenarios**:
  ```
  Scenario: Systemd 服务验证
    Tool: Bash (systemctl)
    Steps:
      1. sudo ./install-bridge.sh
      2. sudo systemctl start zeroclaw-bridge
      3. sudo systemctl status zeroclaw-bridge
    Expected Result: status 显示 "active (running)"
    Evidence: .sisyphus/evidence/task-17-systemd.txt
  ```

  **Commit**: YES
  - Message: `ops(bridge): add systemd service and deployment script`
  - Files: `scripts/zeroclaw-bridge.service`, `scripts/install-bridge.sh`

- [ ] 18. 运维文档（部署、配置、故障排查）

  **What to do**:
  - 创建 `docs/ops/mqtt-bridge-deployment.md`
  - 包含：安装步骤、配置说明、故障排查
  - 添加常见问题 FAQ

  **Recommended Agent Profile**:
  - **Category**: `writing`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 5
  - **Blocks**: F1-F4
  - **Blocked By**: Task 16

  **Acceptance Criteria**:
  - [ ] 文档包含完整部署步骤
  - [ ] 包含至少 5 个故障排查场景

  **QA Scenarios**:
  ```
  Scenario: 文档完整性验证
    Tool: Bash (grep)
    Steps:
      1. grep -i "installation" docs/ops/mqtt-bridge-deployment.md
      2. grep -i "troubleshooting" docs/ops/mqtt-bridge-deployment.md
    Expected Result: 两个 grep 都有匹配
    Evidence: .sisyphus/evidence/task-18-doc-check.txt
  ```

  **Commit**: YES
  - Message: `docs(bridge): add deployment and operations guide`
  - Files: `docs/ops/mqtt-bridge-deployment.md`

- [ ] 19. ESP32 固件示例和文档（含 Berry Script）

  **What to do**:
  - 完善 `firmware/esp32-node/README.md`
  - 添加配置说明（WiFi、MQTT broker）
  - 提供烧录步骤（物理硬件）
  - **添加 Berry Script 使用指南**（重点）
  - **提供 Berry 脚本示例**（GPIO/ADC/传感器）
  - 添加 QEMU 调试指南

  **Must NOT do**:
  - 不要省略 Berry Script 使用说明
  - 不要省略 QEMU 使用说明

  **Recommended Agent Profile**:
  - **Category**: `writing`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 5
  - **Blocks**: F1-F4
  - **Blocked By**: Task 16

  **Acceptance Criteria**:
  - [ ] README 包含完整烧录步骤
  - [ ] 包含配置示例
  - [ ] **包含 Berry Script 完整指南**
  - [ ] **包含至少 3 个 Berry 脚本示例**
  - [ ] 包含 QEMU 调试指南

  **QA Scenarios**:
  ```
  Scenario: README 完整性验证
    Tool: Bash (grep)
    Steps:
      1. grep -i "berry" firmware/esp32-node/README.md
      2. grep -c "```berry" firmware/esp32-node/README.md
    Expected Result: 至少 3 个 Berry 代码示例
    Evidence: .sisyphus/evidence/task-19-readme-berry.txt
  ```

  **Commit**: YES
  - Message: `docs(firmware): add ESP32 firmware guide with Berry Script examples`
  - Files: `firmware/esp32-node/README.md`

---

  **What to do**:
  - 创建 `zeroclaw-bridge.service` systemd 文件
  - 编写安装脚本 `install-bridge.sh`
  - 添加配置文件模板

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4
  - **Blocks**: F1-F4
  - **Blocked By**: Task 13

  **Acceptance Criteria**:
  - [ ] `sudo systemctl start zeroclaw-bridge` 启动成功
  - [ ] `sudo systemctl status zeroclaw-bridge` 显示 active

  **QA Scenarios**:
  ```
  Scenario: Systemd 服务验证
    Tool: Bash (systemctl)
    Steps:
      1. sudo ./install-bridge.sh
      2. sudo systemctl start zeroclaw-bridge
      3. sudo systemctl status zeroclaw-bridge
    Expected Result: status 显示 "active (running)"
    Evidence: .sisyphus/evidence/task-14-systemd.txt
  ```

  **Commit**: YES
  - Message: `ops(bridge): add systemd service and deployment script`
  - Files: `scripts/zeroclaw-bridge.service`, `scripts/install-bridge.sh`

- [ ] 15. 运维文档（部署、配置、故障排查）

  **What to do**:
  - 创建 `docs/ops/mqtt-bridge-deployment.md`
  - 包含：安装步骤、配置说明、故障排查
  - 添加常见问题 FAQ

  **Recommended Agent Profile**:
  - **Category**: `writing`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4
  - **Blocks**: F1-F4
  - **Blocked By**: Task 13

  **Acceptance Criteria**:
  - [ ] 文档包含完整部署步骤
  - [ ] 包含至少 5 个故障排查场景

  **QA Scenarios**:
  ```
  Scenario: 文档完整性验证
    Tool: Bash (grep)
    Steps:
      1. grep -i "installation" docs/ops/mqtt-bridge-deployment.md
      2. grep -i "troubleshooting" docs/ops/mqtt-bridge-deployment.md
    Expected Result: 两个 grep 都有匹配
    Evidence: .sisyphus/evidence/task-15-doc-check.txt
  ```

  **Commit**: YES
  - Message: `docs(bridge): add deployment and operations guide`
  - Files: `docs/ops/mqtt-bridge-deployment.md`

- [ ] 16. ESP32 固件示例和文档（含 QEMU 使用）

  **What to do**:
  - 完善 `firmware/esp32-node/README.md`
  - 添加配置说明（WiFi、MQTT broker）
  - 提供烧录步骤（物理硬件）
  - **添加 QEMU 调试指南**（重点）
  - 提供 QEMU 测试命令示例

  **Must NOT do**:
  - 不要省略 QEMU 使用说明
  - 不要假设用户有物理硬件

  **Recommended Agent Profile**:
  - **Category**: `writing`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4
  - **Blocks**: F1-F4
  - **Blocked By**: Task 13

  **Acceptance Criteria**:
  - [ ] README 包含完整烧录步骤
  - [ ] 包含配置示例
  - [ ] **包含 QEMU 调试完整指南**
  - [ ] 包含 QEMU 测试命令示例

  **QA Scenarios**:
  ```
  Scenario: README 完整性验证
    Tool: Bash (grep)
    Steps:
      1. grep -i "flash" firmware/esp32-node/README.md
      2. grep -i "configuration" firmware/esp32-node/README.md
      3. grep -i "qemu" firmware/esp32-node/README.md
      4. grep -i "debug" firmware/esp32-node/README.md
    Expected Result: 所有 grep 都有匹配
    Evidence: .sisyphus/evidence/task-16-readme-check.txt

  Scenario: QEMU 命令示例验证
    Tool: Bash (grep + count)
    Steps:
      1. grep -c "qemu-system-xtensa" firmware/esp32-node/README.md
    Expected Result: 至少 3 个 QEMU 命令示例
    Evidence: .sisyphus/evidence/task-16-qemu-examples.txt
  ```

  **Commit**: YES
  - Message: `docs(firmware): add ESP32 firmware guide with QEMU debugging`
  - Files: `firmware/esp32-node/README.md`

---

## Final Verification Wave (MANDATORY — after ALL implementation tasks)

> 4 review agents run in PARALLEL. ALL must APPROVE. Rejection → fix → re-run.

- [ ] F1. **Plan Compliance Audit** — `oracle`

  Read the plan end-to-end. For each "Must Have": verify implementation exists (read file, curl endpoint, run command). For each "Must NOT Have": search codebase for forbidden patterns — reject with file:line if found. Check evidence files exist in .sisyphus/evidence/. Compare deliverables against plan.
  
  Output: `Must Have [N/N] | Must NOT Have [N/N] | Tasks [N/N] | VERDICT: APPROVE/REJECT`

- [ ] F2. **Code Quality Review** — `unspecified-high`

  Run `cargo build --release -p zeroclaw-bridge` + `cargo clippy -p zeroclaw-bridge` + `cargo test -p zeroclaw-bridge`. Review all changed files for: `unwrap()` without error handling, empty catches, hardcoded credentials, commented-out code, unused imports. Check AI slop: excessive comments, over-abstraction, generic names (data/result/item/temp).
  
  Output: `Build [PASS/FAIL] | Clippy [PASS/FAIL] | Tests [N pass/N fail] | Files [N clean/N issues] | VERDICT`

- [ ] F3. **Real Manual QA** — `unspecified-high`

  Start from clean state. Execute EVERY QA scenario from EVERY task — follow exact steps, capture evidence. Test cross-task integration (ESP32 → MQTT → Bridge → WebSocket → Gateway). Test edge cases: MQTT broker restart, WebSocket disconnect, duplicate node ID. Save to `.sisyphus/evidence/final-qa/`.
  
  Output: `Scenarios [N/N pass] | Integration [N/N] | Edge Cases [N tested] | VERDICT`

- [ ] F4. **Scope Fidelity Check** — `deep`

  For each task: read "What to do", read actual diff (git log/diff). Verify 1:1 — everything in spec was built (no missing), nothing beyond spec was built (no creep). Check "Must NOT do" compliance. Detect cross-task contamination: Task N touching Task M's files. Flag unaccounted changes.
  
  Output: `Tasks [N/N compliant] | Contamination [CLEAN/N issues] | Unaccounted [CLEAN/N files] | VERDICT`

---

## Commit Strategy

- **Wave 1**: 5 commits (T1-T5 独立提交)
- **Wave 2**: 4 commits (T6-T9 独立提交)
- **Wave 3**: 3 commits (T10-T12 独立提交)
- **Wave 4**: 4 commits (T13-T16 独立提交)

每个 commit 必须通过 `cargo test` 和 `cargo clippy`。

---

## Success Criteria

### Verification Commands
```bash
# 1. Bridge 编译成功
cargo build --release -p zeroclaw-bridge
# Expected: exit code 0

# 2. 所有测试通过
cargo test -p zeroclaw-bridge
# Expected: all tests pass

# 3. ESP32 固件编译（含 Berry）
cd firmware/esp32-node
platformio run
# Expected: exit code 0

# 4. 端到端流程验证（含 Berry 脚本）
cd tests/integration
docker-compose up -d
./test-e2e-berry.sh
# Expected: exit code 0

# 5. Systemd 服务运行
sudo systemctl status zeroclaw-bridge
# Expected: active (running)
```

### Final Checklist
- [ ] All "Must Have" present (including Berry Script)
- [ ] All "Must NOT Have" absent
- [ ] All tests pass
- [ ] Berry Script integration working
- [ ] Documentation complete
- [ ] Zero modifications to ZeroClaw core code

