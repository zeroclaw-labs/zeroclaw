# ZeroClaw Movie Extension

[![English](https://img.shields.io/badge/lang-English-blue.svg)](#english) [![中文](https://img.shields.io/badge/lang-中文-red.svg)](#中文)

[![Crates.io](https://img.shields.io/crates/v/zeroclaw-movie.svg)](https://crates.io/crates/zeroclaw-movie)
[![License](https://img.shields.io/badge/license-Apache2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.70+-orange.svg)](https://www.rust-lang.org/)

---

<a name="english"></a>

## English

Movie information query extension for ZeroClaw AI Agent — query currently playing movies with ratings, director, cast, and plot summaries.

### Features

- 🌏 **Dual Region Support**: China (Douban - free) and International (TMDB - free with registration)
- 🎬 **Now Playing**: Query currently playing movie lists
- 🔍 **Movie Search**: Search movies by name
- ⭐ **Ratings**: Douban and TMDB ratings
- 🎥 **Director & Cast**: Director and top cast information (TMDB)
- 📝 **Plot Summary**: Movie overview / synopsis (TMDB)
- 🖼️ **Poster**: Movie poster URLs
- 🔌 **Easy Integration**: Designed for ZeroClaw AI Agent
- 🆓 **Free to Use**: China region is completely free; US region requires free API key registration

### Quick Start

#### 1. Build

```bash
git clone https://github.com/yourusername/zeroclaw-movie.git
cd zeroclaw-movie
cargo build --release
```

#### 2. Configure API Keys

**China (Douban API) - Completely Free**

No API key required. Works out of the box.

```bash
# Optional: override default Douban endpoint
export DOUBAN_API_URL="https://movie.douban.com"
```

**International (TMDB API) - Free Registration**

1. Visit https://www.themoviedb.org/ and sign up (email only, no credit card)
2. Go to **Settings** → **API** → **Create new API Key**
3. Select "Personal/Non-commercial", submit, and copy the key

```bash
export TMDB_API_KEY="your_tmdb_api_key_here"
```

TMDB API highlights:
- Free for non-commercial use
- World's largest movie database
- Includes director, cast, and plot summaries
- Rate limit: 40 requests / 10 seconds

#### 3. Run Example

```bash
cargo run --example basic_usage
```

### Usage

#### As a Standalone Library

```rust
use zeroclaw_movie::MovieShowtimesTool;

#[tokio::main]
async fn main() {
    let tool = MovieShowtimesTool::new(
        std::env::var("TMDB_API_KEY").ok(),
    ).await.unwrap();

    // Query hot movies (defaults to Douban)
    let result = tool.query_movies(None).await.unwrap();
    println!("{}", result);

    // Search a specific movie
    let result = tool.query_movies(Some("Dune")).await.unwrap();
    println!("{}", result);
}
```

#### Integrate with ZeroClaw

**Option 1: External tool**

```toml
# ~/.zeroclaw/config.toml
[tools.movie_info]
enabled = true
tmdb_api_key = "your_tmdb_api_key"
```

**Option 2: Compile into ZeroClaw**

```toml
# In ZeroClaw's Cargo.toml
[dependencies]
zeroclaw-movie = { path = "../zeroclaw-movie", features = ["zeroclaw-integration"] }
```

```rust
use zeroclaw_movie::MovieShowtimesTool;

pub fn create_tools() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(MovieShowtimesTool::new(
            std::env::var("TMDB_API_KEY").ok()
        ).await.unwrap()),
    ]
}
```

### API Reference

```rust
pub async fn query_movies(
    &self,
    movie_name: Option<&str>,
) -> Result<ToolResult>
```

- `movie_name`: Optional search keyword. If `None`, returns hot/now-playing movies.
- **Auto region detection**: Chinese characters → Douban; English → TMDB; no keyword → Douban by default.

```rust
// Hot movies (Douban)
tool.query_movies(None).await?;

// Search Chinese movie
tool.query_movies(Some("流浪地球")).await?;

// Search English movie (requires TMDB_API_KEY)
tool.query_movies(Some("Dune")).await?;
```

### Output Example

```
✅ Query successful

📽️ TMDB - Now playing movies (20)

1. Dune: Part Two (Dune: Part Two) ⭐8.3 [2024-02-27]
   导演: Denis Villeneuve
   主演: Timothée Chalamet, Zendaya, Rebecca Ferguson, Josh Brolin, Austin Butler
   简介: Follow the mythic journey of Paul Atreides as he unites with Chani and the...

2. Godzilla x Kong: The New Empire ⭐7.1 [2024-03-27]
   导演: Adam Wingard
   主演: Rebecca Hall, Brian Tyree Henry, Dan Stevens, Kaylee Hottle, Alex Ferns
   简介: Two ancient titans, Godzilla and Kong, clash in an epic battle as humans...
...
```

> **Note**: TMDB queries automatically fetch director, top cast, and plot summary for each movie.
> Douban API is limited to title and rating due to web API restrictions.

### Troubleshooting

| Problem | Solution |
|---------|----------|
| Empty results | Check API key (TMDB only), verify movie name spelling, check network |
| TMDB query fails | Verify `TMDB_API_KEY` is set and valid |
| Douban connection error | Douban uses web endpoints; may be affected by network environment |

Enable debug logs:

```bash
RUST_LOG=debug cargo run --example basic_usage
```

### Testing

```bash
cargo test
cargo test --test integration_tests
cargo run --example basic_usage
```

---

<a name="中文"></a>

## 中文

电影信息查询扩展模块 - 为 ZeroClaw AI Agent 提供正在热映电影查询功能，包含评分、导演、主演和剧情简介。

### 功能特性

- 🌏 **双区域支持**: 中国大陆（豆瓣 - 免费）和国际（TMDB - 免费注册）电影数据
- 🎬 **热映电影**: 查询当前正在热映的电影列表
- 🔍 **电影搜索**: 支持按电影名称搜索
- ⭐ **评分信息**: 显示豆瓣评分和 TMDB 评分
- 🎥 **导演主演**: 展示导演和主要演员信息（TMDB）
- 📝 **剧情简介**: 提供电影剧情概述（TMDB）
- 🖼️ **海报信息**: 提供电影海报 URL
- 🔌 **易于集成**: 完美适配 ZeroClaw AI Agent
- 🆓 **免费使用**: 中国区完全免费，美国区免费注册 API key

### 快速开始

#### 1. 安装依赖

```bash
git clone https://github.com/yourusername/zeroclaw-movie.git
cd zeroclaw-movie
cargo build --release
```

#### 2. 配置 API 密钥

**中国区（豆瓣 API）- 完全免费！**

豆瓣 API **无需 API Key**，开箱即用：

```bash
# 可选：覆盖默认豆瓣接口地址
export DOUBAN_API_URL="https://movie.douban.com"
```

说明：
- ✅ 免费使用，无需注册
- ✅ 提供正在热映、搜索等接口
- ✅ 支持中文电影名称搜索

**美国区（TMDB API）- 免费注册**

TMDB (The Movie Database) 提供完全免费的 API，需要简单注册：

1. **访问官网**: https://www.themoviedb.org/
2. **注册账号**: 点击 "Join TMDB"，只需邮箱，无需信用卡
3. **申请 API Key**: 登录后进入 **Settings** → **API** → 点击 "Create new API Key" → 用途选 "Personal/Non-commercial" → 提交后立即可看到 API Key
4. **复制 Key**: 复制 "API Key (v3 auth)" 的值

```bash
export TMDB_API_KEY="your_tmdb_api_key_here"
```

TMDB API 特点：
- ✅ 完全免费（非商业用途）
- ✅ 全球最大电影数据库，数据质量极高
- ✅ 包含导演、主演、剧情简介等详细信息
- ✅ 速率限制宽松：40 次请求 / 10 秒
- ⚠️ 需要提供 API key（但注册免费，立即生效）

如果不配置 TMDB API，仅支持中文电影查询。

#### 3. 运行示例

```bash
cargo run --example basic_usage
```

### 使用方法

#### 作为独立库使用

```rust
use zeroclaw_movie::MovieShowtimesTool;

#[tokio::main]
async fn main() {
    // 中国区使用豆瓣 API（免费，无需 key）
    // 美国区使用 TMDB API（免费注册获得 key）
    let tool = MovieShowtimesTool::new(
        std::env::var("TMDB_API_KEY").ok(),
    ).await.unwrap();

    // 查询当前热映电影
    let result = tool.query_movies(None).await.unwrap();
    println!("{}", result);

    // 搜索特定电影
    let result = tool.query_movies(Some("流浪地球")).await.unwrap();
    println!("{}", result);
}
```

#### 集成到 ZeroClaw

**方式 1: 作为外部工具集成**

```toml
# ~/.zeroclaw/config.toml
[tools.movie_info]
enabled = true
tmdb_api_key = "your_tmdb_api_key"  # 可选，用于国际电影查询
```

**方式 2: 编译进 ZeroClaw**

```toml
# In ZeroClaw's Cargo.toml
[dependencies]
zeroclaw-movie = { path = "../zeroclaw-movie", features = ["zeroclaw-integration"] }
```

```rust
use zeroclaw_movie::MovieShowtimesTool;

pub fn create_tools() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(MovieShowtimesTool::new(
            std::env::var("TMDB_API_KEY").ok()
        ).await.unwrap()),
    ]
}
```

### API 说明

```rust
pub async fn query_movies(
    &self,
    movie_name: Option<&str>,
) -> Result<ToolResult>
```

- `movie_name`: 可选的电影名称搜索关键词。如果为 `None`，返回当前热映电影列表。
- **区域自动识别**: 中文字符 → 豆瓣 API；英文 → TMDB API；无关键词 → 默认豆瓣。

```rust
// 查询当前热映电影（豆瓣）
tool.query_movies(None).await?;

// 搜索中文电影
tool.query_movies(Some("流浪地球")).await?;

// 搜索英文电影（需要 TMDB_API_KEY）
tool.query_movies(Some("Dune")).await?;
```

### 输出示例

```
✅ 查询成功

📽️ TMDB - Now playing movies (20)

1. Dune: Part Two (Dune: Part Two) ⭐8.3 [2024-02-27]
   导演: Denis Villeneuve
   主演: Timothée Chalamet, Zendaya, Rebecca Ferguson, Josh Brolin, Austin Butler
   简介: Follow the mythic journey of Paul Atreides as he unites with Chani and the...

2. Godzilla x Kong: The New Empire ⭐7.1 [2024-03-27]
   导演: Adam Wingard
   主演: Rebecca Hall, Brian Tyree Henry, Dan Stevens, Kaylee Hottle, Alex Ferns
   简介: Two ancient titans, Godzilla and Kong, clash in an epic battle as humans...
...
```

**说明**：
- TMDB 查询自动获取每部电影的导演、主要演员和剧情简介
- 豆瓣 API 受限于 Web 接口，仅返回标题和评分等基本信息

### 故障排除

| 问题 | 解决方案 |
|------|----------|
| 查询返回空结果 | 检查 API key（仅 TMDB 需要）、确认电影名称拼写、检查网络连接 |
| TMDB 查询失败 | 确认 TMDB_API_KEY 已正确设置且有效 |
| 豆瓣 API 无法连接 | 豆瓣使用网页接口，可能受网络环境影响，尝试更换网络 |

启用详细日志：

```bash
RUST_LOG=debug cargo run --example basic_usage
```

### 测试

```bash
cargo test
cargo test --test integration_tests
cargo run --example basic_usage
```

---

## Contributing

1. Fork this repository
2. Create your feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

## License

Apache License 2.0 - see [LICENSE](LICENSE)

## Acknowledgments

- [ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw) - AI Agent Framework
- [Douban](https://movie.douban.com/) - China Movie Data
- [TMDB](https://www.themoviedb.org/) - The Movie Database

---

**Disclaimer**: This project is for educational and non-commercial use. Please comply with the respective API terms of service in production environments.
