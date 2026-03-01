# WASM 工具指南

本指南涵蓋在 ZeroClaw 中建置、安裝及使用基於 WASM 工具（技能）所需的一切。
WASM 工具讓您可以用任何能編譯為 WebAssembly 的語言來擴充 agent 的自訂功能，
而無需修改 ZeroClaw 的核心原始碼。

---

## 目錄

1. [運作原理](#1-運作原理)
2. [前置需求](#2-前置需求)
3. [建立工具](#3-建立工具)
   - [從範本產生骨架](#31-從範本產生骨架)
   - [協定：stdin / stdout](#32-協定stdin--stdout)
   - [manifest.json](#33-manifestjson)
   - [範本：Rust](#34-範本rust)
   - [範本：TypeScript](#35-範本typescript)
   - [範本：Go](#36-範本go)
   - [範本：Python](#37-範本python)
4. [建置](#4-建置)
5. [本地測試](#5-本地測試)
6. [安裝](#6-安裝)
   - [從本地路徑安裝](#61-從本地路徑安裝)
   - [從 git 儲存庫安裝](#62-從-git-儲存庫安裝)
   - [從 ZeroMarket 註冊表安裝](#63-從-zeromarket-註冊表安裝)
7. [ZeroClaw 如何載入和使用工具](#7-zeroclaw-如何載入和使用工具)
8. [目錄結構參考](#8-目錄結構參考)
9. [設定（`[wasm]` 區段）](#9-設定wasm-區段)
10. [安全模型](#10-安全模型)
11. [疑難排解](#11-疑難排解)

---

## 1. 運作原理

```
┌─────────────────────────────────────────────────────────────┐
│  您的 WASM 工具 (.wasm 二進位檔)                              │
│                                                             │
│  stdin  ← 來自 LLM 的 JSON 參數                              │
│  stdout → JSON 結果 { success, output, error }               │
└───────────────────────┬─────────────────────────────────────┘
                        │  WASI stdio 協定
┌───────────────────────▼─────────────────────────────────────┐
│  ZeroClaw WASM 引擎 (wasmtime + WASI)                        │
│                                                             │
│  • 從 skills/ 目錄載入 tool.wasm + manifest.json              │
│  • 將工具註冊到 agent 的工具註冊表                              │
│  • 當 LLM 選擇該工具時執行                                    │
│  • 強制執行記憶體、燃料和輸出大小限制                            │
└─────────────────────────────────────────────────────────────┘
```

核心概念：**無需自訂 SDK 或 ABI 樣板程式碼**。任何能從 stdin 讀取並寫入 stdout
的語言都可以運作。唯一的合約是 [第 2 節](#32-協定stdin--stdout) 中描述的 JSON 格式。

---

## 2. 前置需求

| 需求 | 用途 |
|------|------|
| ZeroClaw 以 `--features wasm-tools` 建置 | 啟用 WASM 執行階段 |
| `wasmtime` CLI | 本地測試（`zeroclaw skill test`） |
| 語言專屬工具鏈 | 從原始碼建置 `.wasm` |

> 注意：Android/Termux 建置目前以 stub 模式執行 `wasm-tools`。
> 請在 Linux/macOS/Windows 上建置以獲得完整的 WASM 執行階段支援。

安裝 `wasmtime` CLI：

```bash
# macOS / Linux
curl https://wasmtime.dev/install.sh -sSf | bash

# 或透過 cargo
cargo install wasmtime-cli
```

在編譯時啟用 WASM 支援：

```bash
cargo build --release --features wasm-tools
```

---

## 3. 建立工具

### 3.1 從範本產生骨架

```bash
zeroclaw skill new <name> --template <typescript|rust|go|python>
```

範例：

```bash
zeroclaw skill new weather_lookup --template rust
```

這會建立新目錄 `./weather_lookup/`，內含所有可立即建置的樣板檔案。
省略 `--template` 旗標時預設使用 `typescript`。

支援的範本：

| 範本 | 執行階段 | 建置工具 |
|------|----------|----------|
| `typescript` | Javy (JS → WASM) | `npm run build` |
| `rust` | 原生 wasm32-wasip1 | `cargo build` |
| `go` | TinyGo | `tinygo build` |
| `python` | componentize-py | `componentize-py` |

---

### 3.2 協定：stdin / stdout

每個 WASM 工具必須遵循此單一合約：

**輸入**（由 ZeroClaw 寫入工具的 stdin）：

```json
{ "param1": "value1", "param2": 42 }
```

輸入物件的格式取決於您在 `manifest.json` 的 `parameters` 中定義的內容。
ZeroClaw 會將 LLM 提供的參數物件原樣傳遞。

**輸出**（由 ZeroClaw 從工具的 stdout 讀取）：

```json
{ "success": true,  "output": "result text shown to LLM", "error": null }
{ "success": false, "output": "",                          "error": "reason" }
```

| 欄位 | 型別 | 必要 | 說明 |
|------|------|------|------|
| `success` | bool | 是 | 工具正常完成時為 `true` |
| `output` | string | 是 | 轉送給 LLM 的結果文字 |
| `error` | string 或 null | 是 | `success` 為 `false` 時的錯誤訊息 |

---

### 3.3 manifest.json

每個工具必須在 `tool.wasm` 旁附帶 `manifest.json`。此檔案告訴 ZeroClaw
工具的名稱、描述，以及其參數的 JSON Schema。

```json
{
  "name": "weather_lookup",
  "description": "Fetches the current weather for a given city name.",
  "version": "1",
  "parameters": {
    "type": "object",
    "properties": {
      "city": {
        "type": "string",
        "description": "City name to look up (e.g. Hanoi, Tokyo)"
      },
      "units": {
        "type": "string",
        "enum": ["metric", "imperial"],
        "description": "Temperature unit system"
      }
    },
    "required": ["city"]
  },
  "homepage": "https://github.com/yourname/weather_lookup"
}
```

| 欄位 | 必要 | 說明 |
|------|------|------|
| `name` | 是 | 暴露給 LLM 的 snake_case 工具名稱 |
| `description` | 是 | 人類可讀的描述（供 LLM 進行工具選擇時顯示） |
| `version` | 否 | 清單檔格式版本，預設為 `"1"` |
| `parameters` | 是 | 工具輸入參數的 JSON Schema |
| `homepage` | 否 | 在 `zeroclaw skill list` 中顯示的選用 URL |

`name` 欄位是 LLM 決定呼叫您的工具時使用的識別符。
請保持其描述性且唯一。

---

### 3.4 範本：Rust

**骨架檔案：** `Cargo.toml`、`src/lib.rs`、`.cargo/config.toml`

`src/lib.rs`：

```rust
use std::io::{self, Read, Write};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct Args {
    city: String,
    #[serde(default)]
    units: String,
}

#[derive(Serialize)]
struct ToolResult {
    success: bool,
    output: String,
    error: Option<String>,
}

fn main() {
    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf).unwrap();

    let result = match serde_json::from_str::<Args>(&buf) {
        Ok(args) => run(args),
        Err(e) => ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!("invalid input: {e}")),
        },
    };

    io::stdout()
        .write_all(serde_json::to_string(&result).unwrap().as_bytes())
        .unwrap();
}

fn run(args: Args) -> ToolResult {
    // 您的邏輯寫在這裡
    ToolResult {
        success: true,
        output: format!("Weather in {}: sunny 28°C", args.city),
        error: None,
    }
}
```

**建置：**

```bash
# 新增目標平台（僅需一次）
rustup target add wasm32-wasip1

# 建置
cargo build --target wasm32-wasip1 --release
cp target/wasm32-wasip1/release/weather_lookup.wasm tool.wasm
```

---

### 3.5 範本：TypeScript

**骨架檔案：** `package.json`、`tsconfig.json`、`src/index.ts`

`src/index.ts`：

```typescript
// 從 stdin 讀取輸入（Javy 提供 Javy.IO）
const input = JSON.parse(
  new TextDecoder().decode(Javy.IO.readSync())
);

function run(args: Record<string, unknown>): string {
  const city = String(args["city"] ?? "");
  // 您的邏輯寫在這裡
  return `Weather in ${city}: sunny 28°C`;
}

try {
  const output = run(input);
  Javy.IO.writeSync(
    new TextEncoder().encode(
      JSON.stringify({ success: true, output, error: null })
    )
  );
} catch (err) {
  Javy.IO.writeSync(
    new TextEncoder().encode(
      JSON.stringify({ success: false, output: "", error: String(err) })
    )
  );
}
```

**建置：**

```bash
# 安裝 Javy：https://github.com/bytecodealliance/javy/releases
npm install
npm run build   # → tool.wasm
```

---

### 3.6 範本：Go

**骨架檔案：** `go.mod`、`main.go`

`main.go`：

```go
package main

import (
    "encoding/json"
    "fmt"
    "io"
    "os"
)

type Args struct {
    City  string `json:"city"`
    Units string `json:"units"`
}

type ToolResult struct {
    Success bool    `json:"success"`
    Output  string  `json:"output"`
    Error   *string `json:"error"`
}

func main() {
    data, _ := io.ReadAll(os.Stdin)
    var args Args
    if err := json.Unmarshal(data, &args); err != nil {
        msg := err.Error()
        out, _ := json.Marshal(ToolResult{Error: &msg})
        os.Stdout.Write(out)
        return
    }
    result := run(args)
    out, _ := json.Marshal(result)
    os.Stdout.Write(out)
}

func run(args Args) ToolResult {
    return ToolResult{
        Success: true,
        Output:  fmt.Sprintf("Weather in %s: sunny 28°C", args.City),
    }
}
```

**建置：**

```bash
# 安裝 TinyGo：https://tinygo.org/getting-started/install/
tinygo build -o tool.wasm -target wasi .
```

---

### 3.7 範本：Python

**骨架檔案：** `app.py`、`requirements.txt`

`app.py`：

```python
import sys
import json

def run(args: dict) -> str:
    city = str(args.get("city", ""))
    # 您的邏輯寫在這裡
    return f"Weather in {city}: sunny 28°C"

def main():
    raw = sys.stdin.read()
    try:
        args = json.loads(raw)
        output = run(args)
        result = {"success": True, "output": output, "error": None}
    except Exception as exc:
        result = {"success": False, "output": "", "error": str(exc)}
    sys.stdout.write(json.dumps(result))

if __name__ == "__main__":
    main()
```

**建置：**

```bash
pip install componentize-py
componentize-py -d wit/ -w zeroclaw-skill componentize app -o tool.wasm
```

---

## 4. 建置

編輯工具邏輯後，將其建置為 `tool.wasm`：

| 範本 | 建置指令 | 輸出 |
|------|----------|------|
| Rust | `cargo build --target wasm32-wasip1 --release && cp target/wasm32-wasip1/release/*.wasm tool.wasm` | `tool.wasm` |
| TypeScript | `npm run build` | `tool.wasm` |
| Go | `tinygo build -o tool.wasm -target wasi .` | `tool.wasm` |
| Python | `componentize-py -d wit/ -w zeroclaw-skill componentize app -o tool.wasm` | `tool.wasm` |

輸出檔案必須命名為 `tool.wasm`，位於技能目錄的根目錄。

---

## 5. 本地測試

安裝之前，可以直接測試工具而不需啟動完整的 ZeroClaw agent：

```bash
zeroclaw skill test . --args '{"city":"Hanoi","units":"metric"}'
```

您也可以透過名稱測試已安裝的技能：

```bash
zeroclaw skill test weather_lookup --args '{"city":"Tokyo"}'
```

或測試多工具技能中的特定工具：

```bash
zeroclaw skill test . --tool my_tool_name --args '{"city":"Paris"}'
```

在底層，`skill test` 透過 stdin 將 JSON 參數傳入 `wasmtime run tool.wasm`
並印出原始的 stdout 回應。這讓您可以快速迭代而不需重啟 agent。

您也可以直接使用 `wasmtime` 手動測試：

```bash
echo '{"city":"Hanoi"}' | wasmtime tool.wasm
```

預期輸出：

```json
{"success":true,"output":"Weather in Hanoi: sunny 28°C","error":null}
```

---

## 6. 安裝

### 6.1 從本地路徑安裝

```bash
zeroclaw skill install ./weather_lookup
```

這會將您的技能目錄複製到 `<workspace>/skills/weather_lookup/`。
ZeroClaw 會在下次啟動時自動發現它。

### 6.2 從 git 儲存庫安裝

```bash
zeroclaw skill install https://github.com/yourname/weather_lookup.git
```

ZeroClaw 會將儲存庫複製到技能目錄中，並掃描 WASM 工具。

### 6.3 從 ZeroMarket 註冊表安裝

```bash
# 格式：namespace/package-name
zeroclaw skill install acme/weather-lookup

# 指定特定版本
zeroclaw skill install acme/weather-lookup@0.2.1
```

ZeroClaw 從設定的註冊表 URL 取得套件索引，然後為套件中的每個工具下載
`tool.wasm` 和 `manifest.json`。

**驗證安裝：**

```bash
zeroclaw skill list
```

---

## 7. ZeroClaw 如何載入和使用工具

### 7.1 啟動時發現

每次 ZeroClaw agent 啟動時，會掃描 `skills/` 目錄並自動載入所有有效的
WASM 工具。安裝後不需要修改設定或重新啟動指令。

```
<workspace>/
└── skills/
    └── weather_lookup/           ← 技能套件根目錄
        ├── SKILL.toml
        └── tools/
            └── weather_lookup/   ← 個別工具目錄
                ├── tool.wasm     ← 編譯後的 WASM 二進位檔
                └── manifest.json ← 工具中繼資料
```

也支援較簡單的「開發佈局」（建置後直接使用時很方便）：

```
<workspace>/
└── skills/
    └── weather_lookup/
        ├── tool.wasm
        └── manifest.json
```

### 7.2 工具註冊

發現後，每個 `WasmTool` 會與內建工具（如 `shell`、`file`、`web_fetch` 等）
一起註冊到 agent 的工具註冊表中。LLM 平等地看到所有已註冊的工具 —
它無法區分內建工具和 WASM 外掛。

### 7.3 LLM 工具選擇

當使用者傳送訊息時，agent 會將完整的工具註冊表（包含所有 WASM 工具）
附加到 LLM 上下文中。LLM 讀取每個工具清單檔中的 `name` 和 `description`，
並根據使用者的請求決定呼叫哪個工具。

對話範例：

```
使用者：現在河內的天氣如何？

Agent：[內部，LLM 選擇工具 "weather_lookup"，參數 {"city":"Hanoi"}]

      ZeroClaw 呼叫 weather_lookup WASM 工具：
        stdin  → {"city":"Hanoi"}
        stdout ← {"success":true,"output":"Weather in Hanoi: sunny 28°C","error":null}

Agent：目前河內的天氣晴朗，氣溫 28°C。
```

### 7.4 呼叫流程

```
LLM 決定呼叫 "weather_lookup"
  │
  ▼
WasmTool::execute(args: JSON)
  │
  ├─ 將參數序列化為 stdin 位元組
  ├─ 啟動 wasmtime WASI 沙箱
  ├─ 將 stdin 寫入 → WASM 程序
  ├─ 從 WASM 程序讀取 stdout ←（上限 1 MiB）
  ├─ 強制執行燃料限制          （約 10 億條指令）
  ├─ 強制執行掛鐘時間限制      （30 秒）
  └─ 反序列化 ToolResult JSON
  │
  ▼
Agent 格式化輸出並回應使用者
```

### 7.5 錯誤處理

如果工具失敗（非零退出碼、無效 JSON、逾時、燃料耗盡），ZeroClaw
會記錄警告並將錯誤回傳給 LLM。Agent 會繼續執行 —
有問題的外掛永遠不會導致程序當掉。

---

## 8. 目錄結構參考

**已安裝佈局**（由 `zeroclaw skill install` 建立）：

```
skills/
└── <skill-name>/
    ├── SKILL.toml                 ← 套件中繼資料（顯示在技能清單中）
    └── tools/
        └── <tool-name>/
            ├── tool.wasm          ← WASM 二進位檔
            └── manifest.json      ← 工具中繼資料
```

**開發佈局**（快速迭代用，`cargo build` 後直接使用）：

```
skills/
└── <skill-name>/
    ├── tool.wasm
    └── manifest.json
```

兩種佈局都會被自動發現。開發時使用開發佈局，發佈時改用已安裝佈局。

---

## 9. 設定（`[wasm]` 區段）

在您的 `zeroclaw.toml` 中新增此區段以調整 WASM 工具行為：

```toml
[wasm]
# 停用所有 WASM 工具（預設：true）
enabled = true

# 每次呼叫的最大記憶體，單位 MiB，範圍 1–256（預設：64）
memory_limit_mb = 64

# CPU 燃料預算 — 大約每條 WASM 指令一個單位（預設：1_000_000_000）
fuel_limit = 1_000_000_000

# `zeroclaw skill install namespace/package` 使用的註冊表 URL
registry_url = "https://registry.zeromarket.dev"
```

停用所有 WASM 工具而不解除安裝：

```toml
[wasm]
enabled = false
```

---

## 10. 安全模型

WASM 工具在由 wasmtime 強制執行的嚴格 WASI 沙箱中執行：

| 限制條件 | 預設值 |
|----------|--------|
| 檔案系統存取 | **拒絕** — 沒有預開啟的目錄 |
| 網路 socket | **拒絕** — 未啟用 WASI 網路 |
| 最大記憶體 | 64 MiB（可設定，最大 256 MiB） |
| 最大 CPU 指令數 | 約 10 億（可設定） |
| 最大掛鐘時間 | 30 秒硬限制 |
| 最大輸出大小 | 1 MiB |
| 註冊表傳輸 | 僅限 HTTPS — HTTP 會被拒絕 |
| 註冊表路徑遍歷 | 工具名稱在寫入磁碟前會進行驗證 |

惡意或有缺陷的 WASM 工具無法：
- 讀取或寫入主機上的檔案
- 建立網路連線
- 存取環境變數
- 消耗無限的 CPU 或記憶體
- 導致 ZeroClaw 程序當掉

---

## 11. 疑難排解

**`WASM tools are not enabled in this build`**

使用功能旗標重新編譯：

```bash
cargo build --release
```

**`skill test` 時找不到 `wasmtime`**

安裝 wasmtime CLI：

```bash
curl https://wasmtime.dev/install.sh -sSf | bash
# 或
cargo install wasmtime-cli
```

**`WASM module must export '_start'`**

您的二進位檔必須編譯為 WASI 可執行檔（不是函式庫）。對於 Rust，確保
您的 `Cargo.toml` **沒有**設定 `crate-type = ["cdylib"]` — 使用預設的
二進位 crate。對於 Go，使用 `tinygo build -target wasi`（不是 `wasm`）。

**`WASM tool wrote nothing to stdout`**

您的工具退出時未寫入 JSON 結果。請檢查您的 `run()` 函式在返回前
是否始終寫入 stdout，包括錯誤路徑。

**工具未出現在 `zeroclaw skill list` 中**

- 驗證 `manifest.json` 與 `tool.wasm` 放在一起
- 驗證 JSON 格式正確：`cat manifest.json | python3 -m json.tool`
- 重啟 agent — 工具在啟動時才會被發現

**`curl failed` 在註冊表安裝時**

確保已安裝 `curl` 且註冊表 URL 使用 HTTPS。自訂註冊表必須
可連線且回傳預期的套件索引 JSON 格式。
