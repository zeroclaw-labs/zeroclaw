# WuKongIM File Download Feature Design

**Date**: 2025-05-13
**Author**: OpenCode Assistant
**Status**: Implemented ✅

## 1. 功能概述

在 WuKongIM 的 type=14 (MARKDOWN) 消息处理中，自动下载非图片文件到用户的 workspace 目录，并将 markdown 中的链接替换为本地路径，使 ZeroClaw 可以直接读取文件而无需重复下载。

**核心特性**：
- 图片：继续转换为 base64（现有逻辑保持不变）
- 非图片文件：下载到 `{workspace_dir}/downloads/`，替换链接为本地路径
- 自动处理文件名冲突（添加序号后缀）
- 文件大小限制：100MB
- 黑名单：可执行文件和脚本文件
- 下载失败时保留原链接并添加错误提示

## 2. 代码变更

### 2.1 `messaging/media.rs` 变更

**重命名**：
- `process_markdown_with_images` → `process_markdown_resources`
- 新增 `workspace_dir: &Path` 参数

**新增函数**：

```rust
/// 提取 markdown 中的所有链接（图片和普通链接）
pub fn extract_markdown_links(text: &str) -> Vec<(String, String, bool)>
// 返回: [(文本, URL, 是否为图片), ...]

/// 检查文件扩展名是否在黑名单中
pub fn is_blocked_extension(filename: &str) -> bool

/// 下载文件到 workspace/downloads/，处理文件名冲突
pub async fn download_file_to_workspace(
    url: &str,
    workspace_dir: &Path
) -> Result<String, String>
// Ok: 本地路径 "/workspace/downloads/filename.pdf"
// Err: 错误描述 "文件超过 100MB 限制"

/// 处理 markdown 资源：图片转 base64，文件下载到本地
pub async fn process_markdown_resources(
    text: &str,
    workspace_dir: &Path
) -> String
```

**新增常量**：

```rust
const FILE_MAX_BYTES: usize = 100 * 1024 * 1024;  // 100MB

const BLOCKED_EXTENSIONS: &[&str] = &[
    "exe", "dll", "bat", "sh", "app", "dmg",  // 可执行文件
    "js", "py", "rb", "php", "pl",             // 脚本文件
];
```

**修改逻辑**：
- `process_markdown_resources` 函数：
  - 图片链接（`![alt](url)`）→ 调用 `download_image_as_base64`（现有逻辑）
  - 普通文件链接（`[text](url)`）→ 调用 `download_file_to_workspace`，替换为本地路径或 `[text](url) [下载失败: reason]`

### 2.2 `messaging/mod.rs` 变更

```rust
// 更新 re-export
pub use media::{
    download_image_as_base64,
    extract_markdown_images,
    process_markdown_resources,  // 替换 process_markdown_with_images
    download_file_to_workspace,  // 新增
    is_blocked_extension,        // 新增
};
```

### 2.3 `channel.rs` 变更

**新增字段**：

```rust
pub struct WuKongIMChannel {
    // ... 现有字段 ...
    pub(crate) workspace_dir: PathBuf,  // 新增
}
```

**修改构造函数**：

```rust
impl WuKongIMChannel {
    pub fn from_config(config: &WuKongIMConfig, workspace_dir: &Path) -> Self {
        Self {
            // ... 现有字段 ...
            workspace_dir: workspace_dir.to_path_buf(),
        }
    }
}
```

**修改消息处理**：

```rust
// 在 listen() 方法中，MARKDOWN 分支
WkMessageType::MARKDOWN => {
    let text = payload_json
        .get("content").and_then(|c| c.get("text")).and_then(|t| t.as_str())
        .unwrap_or("");
    process_markdown_resources(text, &self.workspace_dir).await
}
```

## 3. 数据流

```
收到 type=14 MARKDOWN 消息
    ↓
解析 payload_json.content.text
    ↓
调用 process_markdown_resources(text, workspace_dir)
    ↓
提取所有 markdown 链接（extract_markdown_links）
    ├─ ![alt](url) → 图片链接
    │   └─ download_image_as_base64(url)
    │       ├─ 成功 → [IMAGE:data:mime;base64,...]
    │       └─ 失败 → [图片下载失败]{url}
    └─ [text](url) → 普通链接
        ├─ 检查扩展名是否在黑名单 → 是 → [text](url) [下载失败: 不允许的文件类型]
        └─ 下载文件
            ├─ 成功 → [text](/workspace/downloads/filename)
            └─ 失败 → [text](url) [下载失败: reason]
    ↓
返回处理后的 markdown 内容
    ↓
发送给 ZeroClaw 运行时
```

## 4. 错误处理

**下载失败的错误级别**：
- `tracing::warn!` - 网络错误、HTTP 错误、文件过大、黑名单扩展名
- `tracing::debug!` - 详细的调试信息

**错误信息格式**：
- 文件太大：`[文件名](url) [下载失败: 文件超过 100MB 限制]`
- HTTP 错误：`[文件名](url) [下载失败: HTTP 404]`
- 网络错误：`[文件名](url) [下载失败: 连接超时]`
- 黑名单扩展名：`[文件名](url) [下载失败: 不允许的文件类型]`

**文件名冲突处理**：
- `document.pdf` 已存在 → `document (1).pdf`
- `document (1).pdf` 已存在 → `document (2).pdf`
- 循环检查直到找到可用文件名

## 5. 配置和常量

**常量定义**（硬编码，未来可扩展为可配置）：
- `FILE_MAX_BYTES: usize = 100 * 1024 * 1024` (100MB)
- `BLOCKED_EXTENSIONS: &[&str]` - 黑名单扩展名列表

**配置无需修改**：
- `workspace_dir` 从全局 `Config` 获取，不存储在 `WuKongIMConfig` 中
- 所有下载策略使用硬编码常量

## 6. 测试策略

### 6.1 单元测试（`messaging/media.rs`）

1. **`test_extract_markdown_links`** - 提取各种格式的链接
   - 只有图片链接
   - 只有文件链接
   - 混合链接
   - 无链接

2. **`test_is_blocked_extension`** - 黑名单检测
   - 检测黑名单扩展名（`.exe`, `.js` 等）
   - 允许合法扩展名（`.pdf`, `.txt` 等）

3. **`test_download_file_to_workspace_success`** - 下载成功
   - 下载文件到指定路径
   - 处理文件名冲突（序号后缀）
   - 返回正确路径

4. **`test_download_file_to_workspace_blocked`** - 黑名单拦截
   - 拦截黑名单扩展名文件
   - 返回错误信息

5. **`test_download_file_to_workspace_too_large`** - 文件过大
   - 拒绝超过 100MB 的文件
   - 返回错误信息

6. **`test_process_markdown_resources_mixed`** - 混合处理
   - 图片 → base64
   - 文件 → 本地路径
   - 失败 → 错误提示

### 6.2 集成测试（`channel.rs`）

- 端到端测试 type=14 消息处理流程

## 7. 影响范围

**修改文件**：
- `crates/zeroclaw-channel-wukongim/src/messaging/media.rs`
- `crates/zeroclaw-channel-wukongim/src/messaging/mod.rs`
- `crates/zeroclaw-channel-wukongim/src/channel.rs`

**无需修改**：
- 配置 schema
- 其他 channel 实现
- ZeroClaw 运行时

## 8. 向后兼容性

- 现有图片处理逻辑完全保持不变
- 只是新增了文件下载功能，不破坏现有行为
- 只是重命名 `process_markdown_with_images` → `process_markdown_resources`，内部调用者只有 `channel.rs` 一处

## 9. 安全考虑

- 文件下载限制在 workspace 目录内
- 黑名单阻止危险文件类型
- 文件大小限制防止磁盘空间耗尽
- 所有操作都有日志审计

## 10. 性能影响

- 同步下载文件会增加消息处理延迟（取决于文件大小和网络速度）
- 对于大文件（接近 100MB）可能需要较长时间
- 考虑未来优化：异步下载、缓存机制