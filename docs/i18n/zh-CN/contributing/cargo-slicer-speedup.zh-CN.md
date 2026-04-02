# 使用 cargo-slicer 加速构建

[cargo-slicer](https://github.com/nickel-org/cargo-slicer) 是一个 `RUSTC_WRAPPER`，它在 MIR（中级中间表示，Mid-level Intermediate Representation）层对不可达的库函数进行桩实现，跳过最终二进制永远不会调用的代码的 LLVM 代码生成。

## 基准测试结果

| 环境 | 模式 | 基准时间 | 使用 cargo-slicer | 耗时节省 |
|---|---|---|---|---|
| 48 核服务器 | syn 预分析 | 3分52秒 | 3分31秒 | **-9.1%** |
| 48 核服务器 | MIR 精确模式 | 3分52秒 | 2分49秒 | **-27.2%** |
| 树莓派 4 | syn 预分析 | 25分03秒 | 17分54秒 | **-28.6%** |

所有测量都是干净的 `cargo +nightly build --release`。MIR 精确模式读取实际的编译器 MIR 来构建更准确的调用图，相比基于 syn 的分析的 799 个单体项，它可以桩实现 1060 个单体项。

## CI 集成

工作流 `.github/workflows/ci-build-fast.yml`（尚未实现）旨在与标准版本构建并行运行加速版本构建。它在 Rust 代码变更和工作流变更时触发，不阻塞合并，作为非阻塞检查并行运行。

CI 使用弹性双路径策略：
- **快速路径：** 安装 `cargo-slicer` 和 `rustc-driver` 二进制文件，运行 MIR 精确模式的切片构建。
- **回退路径：** 如果 `rustc-driver` 安装失败（例如由于 nightly `rustc` API 变化），则运行普通的 `cargo +nightly build --release`，而不是让检查失败。

这可以保持检查有用且正常通过，同时在工具链兼容时保留加速能力。

## 本地使用

```bash
# 一次性安装
cargo install cargo-slicer
rustup component add rust-src rustc-dev llvm-tools-preview --toolchain nightly
cargo +nightly install cargo-slicer --profile release-rustc \
  --bin cargo-slicer-rustc --bin cargo_slicer_dispatch \
  --features rustc-driver

# 使用 syn 预分析构建（在 zeroclaw 根目录执行）
cargo-slicer pre-analyze
CARGO_SLICER_VIRTUAL=1 CARGO_SLICER_CODEGEN_FILTER=1 \
  RUSTC_WRAPPER=$(which cargo_slicer_dispatch) \
  cargo +nightly build --release

# 使用 MIR 精确模式构建（更多桩实现，更大节省）
# 步骤 1：生成 .mir-cache（首次构建使用 MIR_PRECISE）
CARGO_SLICER_MIR_PRECISE=1 CARGO_SLICER_WORKSPACE_CRATES=zeroclaw,zeroclaw_robot_kit \
  CARGO_SLICER_VIRTUAL=1 CARGO_SLICER_CODEGEN_FILTER=1 \
  RUSTC_WRAPPER=$(which cargo_slicer_dispatch) \
  cargo +nightly build --release
# 步骤 2：后续构建自动使用 .mir-cache
```

## 工作原理

1. **预分析** 通过 `syn` 扫描工作区源代码，构建跨 crate 调用图（约 2 秒）。
2. **跨 crate 广度优先搜索** 从 `main()` 开始，识别哪些公共库函数是实际可达的。
3. **MIR 桩实现** 将不可达的函数体替换为 `Unreachable` 终止符 —— 单体收集器找不到被调用者，会修剪整个代码生成子树。
4. **MIR 精确模式**（可选）从二进制 crate 的角度读取实际的编译器 MIR，构建真实的调用图，识别更多不可达函数。

不会修改任何源文件。输出的二进制功能完全相同。
