# ZeroClaw 外掛系統

以 [OpenClaw 的外掛系統](https://github.com/openclaw/openclaw) 為藍本，為 Rust 量身打造的外掛架構。

## 概述

外掛系統允許您以自訂工具、鉤子 (hook)、頻道和提供者來擴充 ZeroClaw，而無需修改核心程式碼。外掛會從標準目錄自動發現，在啟動時載入，並透過乾淨的 API 向主機註冊。

## 架構

### 關鍵元件

1. **清單檔** (`zeroclaw.plugin.toml`)：宣告外掛的中繼資料（id、名稱、版本、描述）
2. **Plugin trait**：定義外掛必須實作的介面（`manifest()` + `register()`）
3. **PluginApi**：傳遞給 `register()`，讓外掛可以貢獻工具、鉤子等
4. **發現機制 (Discovery)**：掃描內建、全域和工作區擴充目錄
5. **註冊表 (Registry)**：管理已載入外掛、工具、鉤子與診斷資訊的中央儲存
6. **載入器 (Loader)**：協調 發現 → 過濾 → 註冊 的流程，並隔離錯誤

### 與 OpenClaw 的比較

| OpenClaw (TypeScript)              | ZeroClaw (Rust)                    |
|------------------------------------|------------------------------------|
| `openclaw.plugin.json`             | `zeroclaw.plugin.toml`             |
| `OpenClawPluginDefinition`         | `Plugin` trait                     |
| `OpenClawPluginApi`                | `PluginApi` struct                 |
| `PluginRegistry` (class)           | `PluginRegistry` struct            |
| `discover()` → `load()` → `register()` | `discover_plugins()` → `load_plugins()` |
| Try/catch 隔離                     | `catch_unwind()` panic 隔離        |
| `[plugins]` 設定區段               | `[plugins]` 設定區段               |

## 撰寫外掛

### 1. 建立清單檔

`extensions/hello-world/zeroclaw.plugin.toml`：

```toml
id = "hello-world"
name = "Hello World"
description = "Example plugin demonstrating the ZeroClaw plugin API."
version = "0.1.0"
```

### 2. 實作 Plugin trait

`extensions/hello-world/src/lib.rs`：

```rust
use zeroclaw::plugins::{Plugin, PluginApi, PluginManifest};
use zeroclaw::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;

pub struct HelloWorldPlugin {
    manifest: PluginManifest,
}

impl HelloWorldPlugin {
    pub fn new() -> Self {
        Self {
            manifest: PluginManifest {
                id: "hello-world".into(),
                name: Some("Hello World".into()),
                description: Some("Example plugin".into()),
                version: Some("0.1.0".into()),
                config_schema: None,
            },
        }
    }
}

impl Plugin for HelloWorldPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn register(&self, api: &mut PluginApi) -> anyhow::Result<()> {
        api.logger().info("registering hello-world plugin");
        api.register_tool(Box::new(HelloTool));
        api.register_hook(Box::new(HelloHook));
        Ok(())
    }
}

// 定義你的工具
struct HelloTool;

#[async_trait]
impl Tool for HelloTool {
    fn name(&self) -> &str { "hello" }
    fn description(&self) -> &str { "Greet the user" }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Name to greet" }
            },
            "required": ["name"]
        })
    }
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("world");
        Ok(ToolResult {
            success: true,
            output: format!("Hello, {name}!"),
            error: None,
        })
    }
}

// 定義你的鉤子
struct HelloHook;

#[async_trait]
impl zeroclaw::hooks::HookHandler for HelloHook {
    fn name(&self) -> &str { "hello-world:session-logger" }
    async fn on_session_start(&self, session_id: &str, channel: &str) {
        tracing::info!(plugin = "hello-world", session_id, channel, "session started");
    }
}
```

### 3. 註冊為內建外掛

目前外掛必須編譯進二進位檔案。在 `src/gateway/mod.rs` 或初始化外掛的位置：

```rust
use zeroclaw::plugins::{load_plugins, Plugin};
use hello_world_plugin::HelloWorldPlugin;

let builtin_plugins: Vec<Box<dyn Plugin>> = vec![
    Box::new(HelloWorldPlugin::new()),
];

let registry = load_plugins(&config.plugins, workspace_dir, builtin_plugins);
```

### 4. 在設定中啟用

`~/.zeroclaw/config.toml`：

```toml
[plugins]
enabled = true

[plugins.entries.hello-world]
enabled = true

[plugins.entries.hello-world.config]
greeting = "Howdy"  # 傳遞給外掛的自訂設定
```

## 設定

### 主開關

```toml
[plugins]
enabled = true  # 設為 false 以停用所有外掛載入
```

### 允許清單 / 拒絕清單

```toml
[plugins]
allow = ["hello-world", "my-plugin"]  # 僅載入這些（空 = 全部符合條件者）
deny = ["bad-plugin"]                 # 永不載入這些
```

### 單一外掛設定

```toml
[plugins.entries.my-plugin]
enabled = true

[plugins.entries.my-plugin.config]
api_key = "secret"
timeout_ms = 5000
```

透過 `api.plugin_config()` 在外掛中存取：

```rust
fn register(&self, api: &mut PluginApi) -> anyhow::Result<()> {
    let cfg = api.plugin_config();
    let api_key = cfg.get("api_key").and_then(|v| v.as_str());
    // ...
}
```

## 發現機制

外掛的發現來源：

1. **內建**：編譯進程式的外掛（直接在程式碼中註冊）
2. **全域**：`~/.zeroclaw/extensions/`
3. **工作區**：`<workspace>/.zeroclaw/extensions/`
4. **自訂**：`plugins.load_paths` 中指定的路徑

每個目錄會掃描包含 `zeroclaw.plugin.toml` 的子目錄。

## 錯誤隔離

外掛與主機程式隔離：

- `register()` 中的 panic 會被攔截並記錄為診斷資訊
- `register()` 回傳的錯誤會被記錄，且該外掛會標記為失敗
- 有問題的外掛不會導致 ZeroClaw 當掉

## 外掛 API

### PluginApi 方法

- `register_tool(tool: Box<dyn Tool>)` — 將工具新增至註冊表
- `register_hook(handler: Box<dyn HookHandler>)` — 新增生命週期鉤子
- `plugin_config() -> &toml::Value` — 存取外掛專屬設定
- `logger() -> &PluginLogger` — 取得此外掛範圍的日誌記錄器

### 可用的鉤子

實作 `zeroclaw::hooks::HookHandler`：

- `on_session_start(session_id, channel)`
- `on_session_end(session_id, channel)`
- `on_tool_call(tool_name, args)`
- `on_tool_result(tool_name, result)`

## 未來擴充

- **動態載入**：在執行階段從 `.so`/`.dylib`/`.wasm` 載入外掛（目前需要編譯）
- **熱重載**：不重啟 ZeroClaw 即可重新載入外掛
- **外掛市集**：發現並安裝社群外掛
- **沙箱化**：在隔離的行程或 WASM 中執行不受信任的外掛

## 測試

執行外掛系統測試：

```bash
cargo test --lib plugins
```

## 範例外掛

完整的範例請參見 `extensions/hello-world/`。

## 參考資料

- [OpenClaw Plugin System](https://github.com/openclaw/openclaw/tree/main/src/plugins)
- [Issue #1414](https://github.com/zeroclaw-labs/zeroclaw/issues/1414)
