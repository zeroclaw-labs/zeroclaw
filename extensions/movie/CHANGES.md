# 更新日志

## 📅 2026-03-07 - 重大更新：美国区改用 TMDB API（免费注册）

### 🎯 变更内容

**美国区 API**: MovieGlu → **TMDB (The Movie Database)**

---

### ✨ 新增功能

#### 1. **TMDB API 支持** ✅
- 新增 `src/api/tmdb.rs` - 完整的 TMDB API 实现
- 支持接口：
  - ✅ 正在热映电影查询
  - ✅ 热门电影查询
  - ✅ 电影搜索
  - ✅ 电影详情获取
  - ✅ 高清海报图片（多尺寸）

#### 2. **用户自定义 API Key** 🔑
- 支持用户自行配置 TMDB API Key
- 完全免费，只需简单注册
- 注册地址：https://www.themoviedb.org/settings/api

---

### 💰 费用变化

| 区域 | 之前 | 现在 | 节省 |
|------|------|------|------|
| 中国区 | ¥50-200/月 | ¥0 | 100% |
| 美国区 | $0 (MovieGlu) | $0 (TMDB) | 免费 |
| **总计** | **¥50-200/月** | **¥0** | **¥600-2400/年** |

---

### ⚠️ 重要说明

#### TMDB API 的特点

虽然 TMDB 提供**高质量的电影数据**，但有以下限制：

| 功能 | 是否支持 | 说明 |
|------|---------|------|
| 电影信息 | ✅ | 标题、评分、海报、简介等 |
| 正在热映 | ✅ | 返回当前热映电影列表 |
| 电影搜索 | ✅ | 按关键词搜索电影 |
| 电影详情 | ✅ | 完整详情（类型、时长、评分等） |
| **影院列表** | ❌ | 不提供 |
| **实时排片** | ❌ | 不提供 |
| **票价信息** | ❌ | 不提供 |
| **在线选座** | ❌ | 不提供 |

**当前实现策略**：
- 查询电影信息 → ✅ 返回真实数据（来自 TMDB）
- 显示排片场次 → ⚠️ 返回示例数据（用于演示）

**示例输出**：
```
✅ 查询成功

TMDB found 10 movies matching 'Inception'

🎬 Sample Cinema (TMDB provides movie info only)
   • Inception 14:30 - 16:30 [Sample] $15.00
   • Interstellar 15:20 - 17:45 [Sample] $14.00
```

---

### 🔧 如何获取 TMDB API Key（5 分钟完成）

#### 步骤 1: 注册账号

1. 访问 https://www.themoviedb.org/
2. 点击右上角 "Join TMDB"
3. 填写邮箱、用户名、密码
4. 验证邮箱（无需信用卡）

#### 步骤 2: 申请 API Key

1. 登录后进入 **Settings** → **API**
2. 点击 "Create new API Key" 或 "Request an API Key"
3. 填写基本信息：
   - **Application Name**: 你的项目名称（如 "ZeroClaw Movie"）
   - **Application URL**: 可以填 `http://localhost`
   - **Description**: 简单描述用途
   - **Use Case**: 选择 "Personal/Non-commercial"
4. 提交后立即可看到 API Key

#### 步骤 3: 复制 Key

复制 "**API Key (v3 auth)**" 的值（一串字母数字组合）

#### 步骤 4: 配置环境变量

```bash
export TMDB_API_KEY="your_tmdb_api_key_here"
```

---

### 🚀 测试

#### 方式 1: 仅使用豆瓣 API（无需 TMDB Key）

```bash
cd /Users/guangmang/Documents/企业超跌提醒/zeroclaw-movie
cargo run --example basic_usage
```

#### 方式 2: 启用 TMDB API（推荐）

```bash
# 设置 TMDB API key
export TMDB_API_KEY="your_key_here"

# 运行示例（会同时测试中国和美国的查询）
cargo run --example basic_usage
```

---

### 📋 升级步骤

#### 如果你已经部署了旧版本

1. **更新代码**:
```bash
git pull origin main
```

2. **配置 TMDB API Key**（可选，但强烈推荐）:
```bash
# 免费注册 TMDB 账号获取 key
export TMDB_API_KEY="your_tmdb_api_key"
```

3. **重新编译**:
```bash
cargo build --release
```

4. **重启服务**:
```bash
docker restart zeroclaw
```

---

### 🎯 下一步建议

#### 短期（立即可做）

1. **注册 TMDB 账号**（5 分钟）:
   - 访问：https://www.themoviedb.org/settings/api
   - 完全免费，无需信用卡

2. **配置 API Key**:
```bash
export TMDB_API_KEY="your_key"
```

3. **测试国际查询**:
```bash
cargo run --example basic_usage
```

#### 中期（未来 1-2 周）

1. **如果需要实时排片**:
   - 考虑购买付费 API（参考 `API_COMPARISON.md`）
   - 或自行开发爬虫（需要注意法律风险）

2. **增强错误处理**:
   - 添加 API 失败时的降级逻辑
   - 实现重试机制

3. **性能优化**:
   - 添加结果缓存
   - 并行查询多个数据源

---

### 🐛 已知问题

#### 问题 1: 不提供实时排片

**现象**: 返回的排片是示例数据

**原因**: TMDB 和豆瓣都不提供实时排片数据

**解决**: 
- 仅用于学习和演示
- 生产环境使用付费 API（如聚合数据）

#### 问题 2: TMDB API Key 未配置

**现象**: 查询美国城市时提示 "US API (TMDB) not configured"

**解决**: 
```bash
export TMDB_API_KEY="your_key"
```

---

### 📊 项目文件清单

所有文件都已创建/更新，位于：
```
/Users/guangmang/Documents/企业超跌提醒/zeroclaw-movie/
```

**新增文件**:
- `src/api/tmdb.rs` - TMDB API 实现

**更新文件**:
- `src/tool.rs` - 改用 TMDB API
- `src/config.rs` - 更新配置结构
- `src/api/mod.rs` - 导出 TMDB 模块
- `Cargo.toml` - 添加 urlencoding 依赖
- `README.md` - 更新使用说明和 TMDB 注册指南
- `config.example.toml` - 更新配置示例
- `API_COMPARISON.md` - 更新 API 对比
- `CHANGES.md` - 本文件

---

### 🎉 总结

#### 变更带来的好处

✅ **零成本**: 完全免费（TMDB 只需免费注册）  
✅ **数据质量高**: 豆瓣 + TMDB 都是权威数据库  
✅ **全球覆盖**: 中国（豆瓣）+ 国际（TMDB）  
✅ **易于获取**: TMDB 注册 5 分钟完成，立即生效  
✅ **文档完善**: 提供详细的注册和使用说明  

#### 需要注意的地方

⚠️ **功能限制**: 不提供实时排片数据  
⚠️ **需要注册**: TMDB 需要简单注册（但完全免费）  
⚠️ **商用建议**: 正式产品建议使用付费 API 获取排片数据  

---

**祝你使用愉快！有任何问题欢迎反馈！** 🎬
