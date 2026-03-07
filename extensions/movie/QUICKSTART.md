# 快速开始指南 🚀

5 分钟内完成 ZeroClaw Movie Extension 的配置和使用！

## 第一步：获取 API 密钥（2 分钟）

### 中国区（猫眼）- 可选

猫眼没有官方公开 API，你可以选择：

**选项 A**: 使用第三方服务（推荐）
- 访问：https://apis.netstart.cn/maoyan/
- 注册并获取 API Key

**选项 B**: 跳过中国区
- 如果只查询美国电影，可以不需要

### 美国区（MovieGlu）- 可选

1. 访问 https://developer.movieglu.com/
2. 注册开发者账号
3. 创建新应用
4. 获取 **API Key** 和 **Client ID**

> 💡 提示：如果两个区域都不配置，工具仍然可以创建，但查询时会返回错误提示。

## 第二步：安装和配置（2 分钟）

### 方式 A: 作为独立库使用

```bash
# 1. 克隆或下载项目
cd zeroclaw-movie

# 2. 设置环境变量
export MAOYAN_API_KEY="你的猫眼 API 密钥"
export MOVIEGLU_API_KEY="你的 MovieGlu API 密钥"
export MOVIEGLU_CLIENT_ID="你的 MovieGlu Client ID"

# 3. 运行示例
cargo run --example basic_usage
```

### 方式 B: 集成到 ZeroClaw

在你的 ZeroClaw 项目中：

```bash
# 1. 添加依赖到你的 Cargo.toml
echo 'zeroclaw-movie = { path = "../zeroclaw-movie", features = ["zeroclaw-integration"] }' >> Cargo.toml

# 2. 在代码中初始化
# 参考 ZEROCLAW_INTEGRATION.md
```

## 第三步：测试查询（1 分钟）

### 测试示例代码

创建 `test_query.rs`:

```rust
use zeroclaw_movie::MovieShowtimesTool;

#[tokio::main]
async fn main() {
    let tool = MovieShowtimesTool::new(
        std::env::var("MAOYAN_API_KEY").ok(),
        std::env::var("MOVIEGLU_API_KEY").ok(),
        std::env::var("MOVIEGLU_CLIENT_ID").ok(),
    ).await.unwrap();
    
    // 测试查询
    let result = tool.query_showtimes(
        "北京",
        Some("六道口"),
        None,
        3,
        None,
    ).await.unwrap();
    
    println!("{}", result);
}
```

运行：
```bash
rustc --edition 2021 test_query.rs -L target/debug/deps --extern zeroclaw_movie=target/debug/libzeroclaw_movie.rlib
./test_query
```

或者直接使用示例：
```bash
cargo run --example basic_usage
```

## 预期输出

成功时你会看到类似输出：

```
✅ Movie tool initialized successfully

📍 Example 1: Querying showtimes in Beijing...
✅ 查询成功

在六道口附近找到 2 家影院，共 5 个场次

🎬 示例影院 - 六道口店 (北京市海淀区学清路)
   距离：0.5 km
   • 流浪地球 2 14:30 - 16:45 IMAX 3D ¥68.00
   • 满江红 15:20 - 17:30 激光厅 ¥45.00
```

失败时会看到：

```
❌ China API (Maoyan) not configured [CONFIG_ERROR]
```

这表示需要配置 API 密钥。

## 常见问题速查

### ❓ 编译错误

```bash
error[E0432]: unresolved import `zeroclaw_movie`
```

**解决**: 确保在项目根目录运行 `cargo build`

### ❓ 运行时错误 - API 未配置

```
Error: China API (Maoyan) not configured
```

**解决**: 
```bash
export MAOYAN_API_KEY="your_key"
```

### ❓ 查询返回空结果

可能原因：
- API 密钥无效
- 位置名称不正确
- 时间范围太小

**解决**: 
- 检查 API key 是否正确
- 尝试其他位置（如"北京"而不是具体区域）
- 增加 hours_ahead 参数

## 下一步

完成快速开始后，你可以：

1. 📖 阅读 [README.md](README.md) 了解完整功能
2. 🔧 查看 [ZEROCLAW_INTEGRATION.md](ZEROCLAW_INTEGRATION.md) 学习如何集成到 ZeroClaw
3. 🎨 自定义配置（缓存、过滤、排序等）
4. 🚀 在生产环境部署

## 获取帮助

- 📖 完整文档：查看 README.md
- 🐛 报告问题：[GitHub Issues](https://github.com/yourusername/zeroclaw-movie/issues)
- 💬 社区讨论：ZeroClaw 官方论坛

---

**祝你使用愉快！🎬**
