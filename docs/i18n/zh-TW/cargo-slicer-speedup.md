# 使用 cargo-slicer 加速建置（繁體中文）

[cargo-slicer](https://github.com/nickel-org/cargo-slicer) 是一個 `RUSTC_WRAPPER`，在 MIR 層級將不可達的函式庫函式替換為空殼，跳過最終二進位檔不會呼叫的程式碼的 LLVM codegen。

## 效能測試結果

| 環境 | 模式 | 基準值 | 使用 cargo-slicer | 牆鐘時間節省 |
|---|---|---|---|---|
| 48 核心伺服器 | syn 預分析 | 3m 52s | 3m 31s | **-9.1%** |
| 48 核心伺服器 | MIR 精確模式 | 3m 52s | 2m 49s | **-27.2%** |
| Raspberry Pi 4 | syn 預分析 | 25m 03s | 17m 54s | **-28.6%** |

所有量測均為乾淨的 `cargo +nightly build --release`。MIR 精確模式讀取實際的編譯器 MIR 以建立更精確的呼叫圖，替換 1,060 個 mono item（相比 syn 分析的 799 個）。

## 持續整合整合

工作流程 [`.github/workflows/ci-build-fast.yml`](../../.github/workflows/ci-build-fast.yml) 在標準建置旁平行執行加速 release 建置。它在 Rust 程式碼變更與工作流程變更時觸發，不阻擋合併，作為非阻擋檢查平行執行。

CI 使用彈性的雙路徑策略：
- **快速路徑**：安裝 `cargo-slicer` 加上 `rustc-driver` 二進位檔，執行 MIR 精確的切片建置。
- **降級路徑**：若 `rustc-driver` 安裝失敗（例如因 nightly `rustc` API 偏移），改為執行一般的 `cargo +nightly build --release`，而非讓檢查失敗。

這使檢查在工具鏈相容時保持加速效果，同時確保有用且維持綠色狀態。

## 本機使用

```bash
# 一次性安裝
cargo install cargo-slicer
rustup component add rust-src rustc-dev llvm-tools-preview --toolchain nightly
cargo +nightly install cargo-slicer --profile release-rustc \
  --bin cargo-slicer-rustc --bin cargo_slicer_dispatch \
  --features rustc-driver

# 使用 syn 預分析建置（從 zeroclaw 根目錄）
cargo-slicer pre-analyze
CARGO_SLICER_VIRTUAL=1 CARGO_SLICER_CODEGEN_FILTER=1 \
  RUSTC_WRAPPER=$(which cargo_slicer_dispatch) \
  cargo +nightly build --release

# 使用 MIR 精確分析建置（更多空殼、更大節省）
# 步驟 1：產生 .mir-cache（首次使用 MIR_PRECISE 建置）
CARGO_SLICER_MIR_PRECISE=1 CARGO_SLICER_WORKSPACE_CRATES=zeroclaw,zeroclaw_robot_kit \
  CARGO_SLICER_VIRTUAL=1 CARGO_SLICER_CODEGEN_FILTER=1 \
  RUSTC_WRAPPER=$(which cargo_slicer_dispatch) \
  cargo +nightly build --release
# 步驟 2：後續建置自動使用 .mir-cache
```

## 運作原理

1. **預分析**透過 `syn` 掃描工作區原始碼，建立跨 crate 呼叫圖（約 2 秒）。
2. **跨 crate BFS** 從 `main()` 出發，識別哪些公開函式庫函式實際可達。
3. **MIR 空殼替換**將不可達的函式主體替換為 `Unreachable` 終結器 — mono 收集器找不到被呼叫者，從而剪除整個 codegen 子樹。
4. **MIR 精確模式**（選用）從二進位 crate 的角度讀取實際的編譯器 MIR，建立真實的呼叫圖，識別更多不可達函式。

不修改任何原始檔。輸出的二進位檔在功能上完全相同。
