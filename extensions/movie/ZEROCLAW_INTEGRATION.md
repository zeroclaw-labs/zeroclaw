# ZeroClaw 集成指南

本指南详细说明如何将 `zeroclaw-movie` 扩展模块集成到 ZeroClaw AI Agent 中。

## 目录

1. [快速集成](#快速集成)
2. [配置方式](#配置方式)
3. [工具注册](#工具注册)
4. [使用示例](#使用示例)
5. [高级配置](#高级配置)
6. [故障排除](#故障排除)

---

## 快速集成

### 步骤 1: 添加依赖

在 ZeroClaw 项目的 `Cargo.toml` 中添加：

```toml
[dependencies]
zeroclaw-movie = { git = "https://github.com/yourusername/zeroclaw-movie.git", branch = "main" }
# 或者使用本地路径
# zeroclaw-movie = { path = "../zeroclaw-movie", features = ["zeroclaw-integration"] }
```

### 步骤 2: 初始化工具

在 ZeroClaw 的工具初始化代码中添加电影查询工具：

```rust
// 在 src/tools/mod.rs 或类似文件中
use zeroclaw_movie::MovieShowtimesTool;

pub async fn initialize_tools() -> Result<Vec<Box<dyn Tool>>> {
    let mut tools: Vec<Box<dyn Tool>> = vec![
        // ... 其他工具
    ];
    
    // 添加电影查询工具
    if cfg!(feature = "movie-extension") {
        let movie_tool = MovieShowtimesTool::new(
            std::env::var("MAOYAN_API_KEY").ok(),
            std::env::var("MOVIEGLU_API_KEY").ok(),
            std::env::var("MOVIEGLU_CLIENT_ID").ok(),
        ).await?;
        
        tools.push(Box::new(movie_tool));
        log::info!("✅ Movie showtimes tool initialized");
    }
    
    Ok(tools)
}
```

### 步骤 3: 配置环境变量

在 `.env` 文件或系统环境中添加：

```bash
# 电影查询扩展配置
MAOYAN_API_KEY=your_maoyan_api_key
MOVIEGLU_API_KEY=your_movieglu_api_key
MOVIEGLU_CLIENT_ID=your_movieglu_client_id
```

---

## 配置方式

### 方式 1: 环境变量（推荐）

最简单的方式是使用环境变量：

```bash
# ~/.bashrc 或 ~/.zshrc
export MAOYAN_API_KEY="sk_xxxxxx"
export MOVIEGLU_API_KEY="mg_xxxxxx"
export MOVIEGLU_CLIENT_ID="client_xxxxxx"
```

### 方式 2: ZeroClaw 配置文件

在 ZeroClaw 的配置文件中添加电影扩展配置：

```toml
# ~/.zeroclaw/config.toml

[extensions.movie]
enabled = true

# 中国区 API
[extensions.movie.china]
api_key = "your_maoyan_key"
enabled = true

# 美国区 API
[extensions.movie.us]
api_key = "your_movieglu_key"
client_id = "your_movieglu_client_id"
timezone = "America/New_York"

# 默认参数
[extensions.movie.defaults]
hours_ahead = 3
max_results = 50
```

### 方式 3: 独立配置文件

创建专门的配置文件 `~/.zeroclaw/movie_config.toml`:

```toml
# 启用状态
enabled = true

# 中国区配置
[china]
enabled = true
api_key = "your_maoyan_key"
api_url = "https://api.maoyan.com"

# 美国区配置
[us]
enabled = true
api_key = "your_movieglu_key"
client_id = "your_movieglu_client_id"
api_url = "https://api.movieglu.com"
timezone = "America/New_York"

# 搜索默认值
[defaults]
hours_ahead = 3
max_results_per_cinema = 10
max_total_results = 50
```

然后在 ZeroClaw 中加载：

```rust
use zeroclaw_movie::MovieConfig;

let config = MovieConfig::from_file("~/.zeroclaw/movie_config.toml")?;

if config.enabled {
    let movie_tool = MovieShowtimesTool::new(
        config.china.api_key,
        config.us.api_key,
        config.us.client_id,
    ).await?;
    
    // 注册工具...
}
```

---

## 工具注册

### 自动注册（使用 Feature）

启用 `zeroclaw-integration` feature 后，工具会自动实现 ZeroClaw 的 `Tool` trait：

```toml
# Cargo.toml
[dependencies]
zeroclaw-movie = { path = "../zeroclaw-movie", features = ["zeroclaw-integration"] }
```

### 手动注册

如果需要更灵活的控制，可以手动注册：

```rust
// src/main.rs 或 src/lib.rs
use zeroclaw_movie::{MovieShowtimesTool, MovieConfig};
use zeroclaw::tools::registry::ToolRegistry;

async fn register_movie_extension(registry: &mut ToolRegistry) -> anyhow::Result<()> {
    // 加载配置
    let config = MovieConfig::from_env();
    
    if !config.enabled {
        log::info!("Movie extension is disabled");
        return Ok(());
    }
    
    // 创建工具
    let movie_tool = MovieShowtimesTool::new(
        config.china.api_key,
        config.us.api_key,
        config.us.client_id,
    ).await?;
    
    // 注册到工具注册表
    registry.register("get_movie_showtimes", Box::new(movie_tool));
    
    log::info!("🎬 Movie extension registered successfully");
    Ok(())
}
```

---

## 使用示例

### 用户对话示例

配置完成后，用户可以通过钉钉（或其他渠道）与 ZeroClaw 对话：

**用户**: 
```
请帮我查下北京六道口附近电影院最近 3 个小时的电影场次
```

**ZeroClaw 回复**:
```
✅ 查询成功

在六道口附近找到 2 家影院，共 5 个场次

🎬 示例影院 - 六道口店 (北京市海淀区学清路)
   距离：0.5 km
   • 流浪地球 2 14:30 - 16:45 IMAX 3D ¥68.00
   • 满江红 15:20 - 17:30 激光厅 ¥45.00
   • 深海 16:00 - 18:10 3D ¥55.00

🎬 万达影城 - 中关村店 (北京市海淀区中关村大街)
   距离：1.2 km
   • 流浪地球 2 15:00 - 17:15 IMAX ¥72.00
   • 熊出没·伴我"熊芯" 14:45 - 16:30 ¥35.00
```

### 更多查询示例

**查询特定电影**:
```
我想看流浪地球 2，查查北京中关村附近的场次
```

**查询美国城市**:
```
Check movie showtimes near Manhattan, New York in the next 5 hours
```

**查询指定日期**:
```
帮我查一下明天下午上海浦东的电影院
```

---

## 高级配置

### 自定义 API 端点

如果需要使用自定义或代理 API 端点：

```toml
# movie_config.toml
[china]
api_url = "https://your-proxy.com/maoyan"

[us]
api_url = "https://your-proxy.com/movieglu"
```

### 区域优先级

如果同时启用了多个区域的 API，可以设置优先级：

```rust
use zeroclaw_movie::RegionPriority;

let priority = RegionPriority::new(vec!["CN", "US"]); // 优先中国区

let movie_tool = MovieShowtimesTool::new_with_priority(
    maoyan_key,
    movieglu_key,
    movieglu_client_id,
    priority,
).await?;
```

### 结果过滤和排序

自定义结果展示：

```rust
use zeroclaw_movie::{ShowtimeFilter, SortBy};

let filter = ShowtimeFilter::new()
    .with_min_price(30.0)      // 最低票价
    .with_max_price(100.0)     // 最高票价
    .with_version("IMAX")      // 只要 IMAX
    .sort_by(SortBy::Price);   // 按价格排序

let result = tool.query_with_filter(query, filter).await?;
```

### 缓存配置

启用结果缓存减少 API 调用：

```toml
# movie_config.toml
[cache]
enabled = true
ttl_seconds = 300  # 5 分钟缓存
max_size = 100     # 最多缓存 100 个查询结果
```

---

## 故障排除

### 检查工具是否注册成功

在 ZeroClaw 启动日志中查找：

```
🎬 Movie extension registered successfully
✅ Movie showtimes tool initialized
```

### 测试工具调用

使用 ZeroClaw 的调试模式测试工具：

```bash
RUST_LOG=debug zeroclaw --test-tool get_movie_showtimes '{"city":"北京","location":"六道口","hours_ahead":3}'
```

### 常见错误

#### 错误 1: API Key 未配置

```
Error: China API (Maoyan) not configured
```

**解决**: 确保环境变量正确设置
```bash
export MAOYAN_API_KEY="your_key"
```

#### 错误 2: 工具未找到

```
Error: Tool 'get_movie_showtimes' not found
```

**解决**: 检查工具是否正确注册到 ZeroClaw 的工具列表中

#### 错误 3: 查询返回空结果

可能原因：
- API 密钥无效或过期
- 位置名称拼写错误
- 时间范围太小
- 该区域没有电影院数据

**解决**:
```bash
# 启用详细日志
export RUST_LOG=zeroclaw_movie=debug

# 重新运行查询
```

### 性能优化

如果查询速度慢：

1. **并行查询多个影院**:
```rust
// 工具内部已实现并行查询
let results = futures::future::join_all(cinema_queries).await;
```

2. **增加超时时间**:
```toml
# movie_config.toml
[http]
timeout_seconds = 60
retry_count = 3
```

3. **使用缓存**:
```toml
[cache]
enabled = true
ttl_seconds = 600
```

---

## 贡献给 ZeroClaw

如果你想将电影扩展贡献到 ZeroClaw 主仓库：

1. **Fork ZeroClaw 仓库**
2. **创建特性分支**:
```bash
git checkout -b feature/movie-extension
```

3. **添加你的修改**:
   - 在 `src/tools/` 中添加 `movie_showtimes.rs`
   - 在 `src/tools/mod.rs` 中注册
   - 更新文档

4. **提交 PR**:
```bash
git commit -m "feat: add movie showtimes query tool

- Support China (Maoyan) and US (MovieGlu) APIs
- Query by city and location
- Filter by movie name and time range
- Display cinema info, showtimes, and prices"
```

---

## 支持

遇到问题？

- 📖 查看 [README.md](README.md) 了解基本用法
- 🐛 提交 [GitHub Issue](https://github.com/yourusername/zeroclaw-movie/issues)
- 💬 加入 ZeroClaw 社区讨论

---

**祝你使用愉快！🎬**
