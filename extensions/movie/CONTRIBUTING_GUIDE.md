# 贡献指南 - 如何向 ZeroClaw 提交电影扩展

本指南将带你一步步完成将 `zeroclaw-movie` 扩展贡献到 ZeroClaw 主仓库的完整流程。

## 📋 目录

1. [准备工作](#准备工作)
2. [方案选择](#方案选择)
3. [实施步骤](#实施步骤)
4. [提交 PR](#提交-pr)
5. [后续维护](#后续维护)

---

## 🛠️ 准备工作

### 1. Fork ZeroClaw 仓库

```bash
# 访问 https://github.com/zeroclaw-labs/zeroclaw
# 点击 Fork 按钮
```

### 2. 克隆你的 Fork

```bash
git clone https://github.com/YOUR_USERNAME/zeroclaw.git
cd zeroclaw
```

### 3. 添加上游远程仓库

```bash
git remote add upstream https://github.com/zeroclaw-labs/zeroclaw.git
git fetch upstream
```

### 4. 创建特性分支

```bash
git checkout -b feature/movie-showtimes-extension
```

---

## 🎯 方案选择

你有 **3 种方案** 可以将电影扩展集成到 ZeroClaw：

### 方案 A: 外部依赖包（推荐 ⭐⭐⭐⭐⭐）

**优点**:
- ✅ 代码解耦，易于维护
- ✅ 可独立发布和版本管理
- ✅ 不影响 ZeroClaw 主仓库结构
- ✅ 用户按需安装

**缺点**:
- ⚠️ 需要额外的配置步骤

**适用场景**: 希望保持扩展独立性，快速迭代

**实现方式**:
```toml
# 在 ZeroClaw 的 Cargo.toml 中添加
[dependencies]
zeroclaw-movie = { git = "https://github.com/YOUR_USERNAME/zeroclaw-movie.git", branch = "main" }
```

### 方案 B: 内置工具模块

**优点**:
- ✅ 开箱即用，无需额外配置
- ✅ 用户体验最佳
- ✅ 完全集成到 ZeroClaw 生态

**缺点**:
- ⚠️ 增加主仓库复杂度
- ⚠️ 需要维护更多依赖关系

**适用场景**: 功能成熟稳定后，作为官方功能提供

**实现方式**:
```
src/tools/
├── movie_showtimes.rs      # 新增
├── mod.rs                  # 修改，添加模块引用
└── ...
```

### 方案 C: 可选 Feature

**优点**:
- ✅ 编译时可选，不增加二进制大小
- ✅ 灵活性高

**缺点**:
- ⚠️ 配置相对复杂

**适用场景**: 高级用户定制需求

**实现方式**:
```toml
# ZeroClaw 的 Cargo.toml
[features]
movie-extension = ["zeroclaw-movie"]
```

---

## 🚀 实施步骤

### 选择方案 A（外部依赖包）的实施步骤

#### Step 1: 准备 zeroclaw-movie 仓库

```bash
# 确保你的 zeroclaw-movie 项目已经：
# 1. 推送到 GitHub
# 2. 有完整的 README.md
# 3. 有 LICENSE 文件
# 4. 能通过 cargo test

cd /Users/guangmang/Documents/企业超跌提醒/zeroclaw-movie
git init
git add .
git commit -m "Initial commit: Movie showtimes extension for ZeroClaw"

# 在 GitHub 创建新仓库并推送
git remote add origin https://github.com/YOUR_USERNAME/zeroclaw-movie.git
git push -u origin main
```

#### Step 2: 修改 ZeroClaw 的依赖配置

编辑 ZeroClaw 项目的 `Cargo.toml`:

```toml
# 在 [dependencies] 部分添加
zeroclaw-movie = { git = "https://github.com/YOUR_USERNAME/zeroclaw-movie.git", branch = "main", optional = true }

# 在 [features] 部分添加
[features]
default = []
movie-extension = ["zeroclaw-movie"]
```

#### Step 3: 添加工具初始化代码

创建或编辑 `src/extensions/movie_extension.rs`:

```rust
//! Movie Extension Integration for ZeroClaw

use anyhow::Result;
use log::{info, warn};

#[cfg(feature = "movie-extension")]
use zeroclaw_movie::{MovieShowtimesTool, MovieConfig};

/// Initialize movie extension if enabled
pub async fn initialize_movie_extension() -> Result<Option<MovieShowtimesTool>> {
    #[cfg(feature = "movie-extension")]
    {
        // Load configuration
        let config = MovieConfig::from_env();
        
        if !config.enabled {
            info!("Movie extension is disabled in configuration");
            return Ok(None);
        }
        
        // Check if API keys are configured
        let has_china_api = config.china.api_key.is_some();
        let has_us_api = config.us.api_key.is_some() && config.us.client_id.is_some();
        
        if !has_china_api && !has_us_api {
            warn!("Movie extension enabled but no API keys configured");
            warn!("Please set MAOYAN_API_KEY or MOVIEGLU_API_KEY environment variables");
            return Ok(None);
        }
        
        // Create tool
        let tool = MovieShowtimesTool::new(
            config.china.api_key,
            config.us.api_key,
            config.us.client_id,
        ).await?;
        
        info!("🎬 Movie extension initialized successfully");
        info!("   China API (Maoyan): {}", if has_china_api { "✅" } else { "❌" });
        info!("   US API (MovieGlu): {}", if has_us_api { "✅" } else { "❌" });
        
        Ok(Some(tool))
    }
    
    #[cfg(not(feature = "movie-extension"))]
    {
        Ok(None)
    }
}
```

#### Step 4: 注册到工具系统

编辑 `src/tools/mod.rs`，在工具初始化函数中调用：

```rust
// 添加模块引用
pub mod extensions;
use extensions::movie_extension;

// 在 initialize_tools() 函数中
pub async fn initialize_tools(config: &Config) -> Result<ToolRegistry> {
    let mut registry = ToolRegistry::new();
    
    // ... 其他工具初始化 ...
    
    // 添加电影扩展
    if let Some(movie_tool) = movie_extension::initialize_movie_extension().await? {
        registry.register_tool(Box::new(movie_tool));
    }
    
    Ok(registry)
}
```

#### Step 5: 更新文档

在 ZeroClaw 的 `README.md` 或 `docs/features.md` 中添加：

```markdown
## 可选扩展

### 电影查询扩展 🎬

查询电影院排片和场次信息，支持中国（猫眼）和美国（MovieGlu）数据。

**安装**:

```bash
# 启用 movie-extension 特性编译
cargo build --features movie-extension

# 或使用 Rust 1.60+
cargo build --all-features
```

**配置**:

```bash
export MAOYAN_API_KEY="your_key"
export MOVIEGLU_API_KEY="your_key"
export MOVIEGLU_CLIENT_ID="your_client_id"
```

**使用示例**:

```
请帮我查下北京六道口附近电影院最近 3 个小时的电影场次
```
```

#### Step 6: 添加环境变量示例

在 `.env.example` 文件中添加：

```bash
# Movie Extension (Optional)
MOVIE_EXTENSION_ENABLED=true
MAOYAN_API_KEY=
MOVIEGLU_API_KEY=
MOVIEGLU_CLIENT_ID=
```

---

## 📤 提交 PR

### 1. 测试你的修改

```bash
# 编译测试
cargo build --features movie-extension

# 运行测试
cargo test --features movie-extension

# 格式化代码
cargo fmt

# Clippy 检查
cargo clippy --features movie-extension -- -D warnings
```

### 2. 提交 Commit

```bash
git add .
git commit -m "feat: add movie showtimes query extension

Features:
- Query movie showtimes in China (via Maoyan API) and US (via MovieGlu API)
- Search by city, location, and time range
- Filter by movie name
- Display cinema info, showtimes, and prices

Implementation:
- Add zeroclaw-movie as optional dependency
- Add movie-extension feature flag
- Implement tool initialization and registration
- Add comprehensive documentation

Usage:
- Enable with: cargo build --features movie-extension
- Configure API keys via environment variables
- Query via chat: '查下北京六道口附近的电影场次'

Related: #ISSUE_NUMBER (if applicable)"
```

### 3. 推送到你的 Fork

```bash
git push origin feature/movie-showtimes-extension
```

### 4. 创建 Pull Request

1. 访问 https://github.com/zeroclaw-labs/zeroclaw/pulls
2. 点击 "New pull request"
3. 选择你的分支 `feature/movie-showtimes-extension`
4. 填写 PR 描述（参考下面的模板）

### PR 描述模板

```markdown
## 🎬 功能描述

添加电影场次查询扩展，支持用户通过自然语言查询附近电影院的排片信息。

## ✨ 主要特性

- [x] 支持中国区（猫眼 API）和美國区（MovieGlu API）
- [x] 按城市、具体位置搜索
- [x] 时间范围查询（如"最近 3 小时"）
- [x] 按电影名称过滤
- [x] 显示影院、场次、票价等详细信息
- [x] 完整的中文和英文文档

## 🔧 技术实现

- 使用外部依赖包方式集成 (`zeroclaw-movie`)
- 可选特性标志 `movie-extension`
- 通过环境变量配置 API 密钥
- 自动检测查询区域（中文城市名 → 中国 API）

## 📚 文档

- [x] README.md - 完整使用说明
- [x] QUICKSTART.md - 5 分钟快速开始
- [x] ZEROCLAW_INTEGRATION.md - 集成指南
- [x] 代码注释完整

## 🧪 测试

```bash
# 编译测试
cargo build --features movie-extension

# 单元测试
cargo test --features movie-extension

# 代码质量
cargo fmt --check
cargo clippy --features movie-extension
```

## 📸 使用示例

**用户**: 请帮我查下北京六道口附近电影院最近 3 个小时的电影场次

**助手**: 
```
✅ 查询成功

在六道口附近找到 2 家影院，共 5 个场次

🎬 示例影院 - 六道口店 (北京市海淀区学清路)
   距离：0.5 km
   • 流浪地球 2 14:30 - 16:45 IMAX 3D ¥68.00
   • 满江红 15:20 - 17:30 激光厅 ¥45.00
```

## 📝 检查清单

- [x] 代码通过所有测试
- [x] 代码格式符合 Rust 规范
- [x] 无 Clippy 警告
- [x] 添加了必要的文档
- [x] 更新了 .env.example
- [ ] 等待 Review 反馈

## 🙏 后续计划

- [ ] 添加更多 API 提供商支持（如豆瓣）
- [ ] 实现结果缓存优化性能
- [ ] 支持在线选座和购票
- [ ] 添加电影评分和评论信息
```

### 5. 等待 Review

- 关注 PR 评论
- 及时回复和维护者的问题
- 根据反馈进行修改

---

## 🔧 后续维护

### 处理 Issue

- 及时回复用户的问题
- 修复报告的 Bug
- 收集功能建议

### 持续改进

- 定期同步 ZeroClaw 主仓库的变更
- 更新依赖版本
- 优化性能和用户体验

### 版本发布

```bash
# 语义化版本号
# MAJOR.MINOR.PATCH

# 更新 Cargo.toml 中的版本号
# 创建 Git tag
git tag -a v0.1.0 -m "Release version 0.1.0"
git push origin v0.1.0
```

---

## 📞 获取帮助

如果在贡献过程中遇到问题：

1. 查看 ZeroClaw 的 [CONTRIBUTING.md](https://github.com/zeroclaw-labs/zeroclaw/blob/master/CONTRIBUTING.md)
2. 在 Discussions 中提问
3. 联系维护者

---

**感谢你的贡献！🎉**

你的工作将帮助更多用户使用 ZeroClaw 查询电影场次！
