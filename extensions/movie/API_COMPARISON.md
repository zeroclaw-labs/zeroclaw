# 电影API 对比与选择指南（2026 最新版）

## 📊 API 免费政策对比

### 中国区 API

| API 提供商 | 是否免费 | API Key | 数据质量 | 实时排片 | 备注 |
|-----------|---------|---------|---------|---------|------|
| **豆瓣** | ✅ **完全免费** | ❌ 不需要 | ⭐⭐⭐⭐ | ❌ 不支持 | 官方页面接口，推荐用于学习 |
| 猫眼 | ❌ 付费 | ✅ 需要 | ⭐⭐⭐⭐⭐ | ✅ 支持 | 第三方服务，约 ¥50-200/月 |
| 淘票票 | ❌ 需申请 | ✅ 需要 | ⭐⭐⭐⭐⭐ | ✅ 支持 | 阿里开放平台，审核严格 |

### 美国区 API

| API 提供商 | 是否免费 | API Key | 数据质量 | 实时排片 | 备注 |
|-----------|---------|---------|---------|---------|------|
| **TMDB** | ✅ **免费注册** | ✅ 需要注册 | ⭐⭐⭐⭐⭐ | ❌ 不支持 | 全球最大电影数据库，强烈推荐 |
| MovieGlu | ❌ 商业授权 | ✅ 需要 | ⭐⭐⭐⭐⭐ | ✅ 支持 | 需要商务洽谈 |

---

## 🎯 推荐配置（零成本方案）⭐⭐⭐⭐⭐

```bash
# 中国区：豆瓣 API（完全免费，无需 key）
export DOUBAN_API_URL="https://movie.douban.com"

# 美国区：TMDB API（免费注册，5 分钟搞定）
export TMDB_API_KEY="your_tmdb_api_key_here"
```

**优点**：
- ✅ 完全免费或免费注册
- ✅ 数据质量高（豆瓣 + TMDB 都是权威数据库）
- ✅ 配置简单，文档完善

**缺点**：
- ⚠️ 不提供实时排片（仅电影信息）
- ⚠️ TMDB 需要简单注册（但完全免费）

---

## 🔍 详细分析

### 豆瓣 API（中国区 - 当前使用）✅

**数据来源**: `https://movie.douban.com`（豆瓣官方页面接口）

**可用接口**:
```bash
# 按标签搜索（热门、新片等）
GET https://movie.douban.com/j/search_subjects?type=movie&tag=热门&page_limit=20

# 关键词搜索建议
GET https://movie.douban.com/j/subject_suggest?q=流浪地球
```

**返回数据示例**:
```json
{
  "subjects": [
    {
      "id": "35267208",
      "title": "流浪地球 2",
      "rate": "8.3",
      "cover": "https://img9.doubanio.com/...jpg",
      "url": "https://movie.douban.com/subject/35267208/"
    }
  ]
}
```

**限制**:
- ❌ 不提供影院列表
- ❌ 不提供实时排片
- ❌ 不提供票价信息
- ✅ 稳定性好（豆瓣官方接口）

**适用场景**:
- ✅ 学习和开发测试
- ✅ 电影信息查询
- ✅ 个人项目、原型开发

---

### TMDB API（美国区 - 当前使用）✅

**官方网站**: https://www.themoviedb.org/

**注册流程**（5 分钟完成）:
1. 访问官网，点击 "Join TMDB" 注册账号（只需邮箱，无需信用卡）
2. 登录后进入 **Settings** → **API**
3. 点击 "Create new API Key" 或 "Request an API Key"
4. 填写基本信息：
   - Application Name: 你的项目名称
   - Application URL: 可以填 localhost
   - Description: 简单描述用途（选 Personal/Non-commercial）
5. 提交后立即可看到 API Key
6. 复制 "API Key (v3 auth)" 的值

**可用接口**:
```bash
# 正在热映
GET https://api.themoviedb.org/3/movie/now_playing?api_key=YOUR_KEY

# 热门电影
GET https://api.themoviedb.org/3/movie/popular?api_key=YOUR_KEY

# 搜索电影
GET https://api.themoviedb.org/3/search/movie?query=inception&api_key=YOUR_KEY

# 电影详情
GET https://api.themoviedb.org/3/movie/157336?api_key=YOUR_KEY
```

**返回数据**:
- ✅ 电影信息（标题、概述、海报）
- ✅ 评分和投票数
- ✅ 上映日期
- ✅ 类型、时长
- ✅ 高清海报图片（多个尺寸）
- ❌ 不提供实时排片
- ❌ 不提供影院信息

**速率限制**:
- 40 次请求 / 10 秒（非常宽松）
- 每日请求上限：无明确限制（合理使用即可）

**适用场景**:
- ✅ 生产环境（非商业用途）
- ✅ 需要高质量电影数据
- ✅ 国际电影查询

---

## 💡 替代方案（如果需要实时排片）

### 中国区付费 API

如果项目需要**实时排片、票价、选座**等功能：

**选项 1**: 聚合数据
- 网址：https://www.juhe.cn/
- 价格：约 ¥100-300/月
- 功能：影院列表、排片、票价

**选项 2**: APISpace
- 网址：https://www.apispace.com/
- 价格：按量付费或包月
- 功能：完整的票务信息

**选项 3**: 万维易源
- 网址：https://www.showapi.com/
- 价格：约 ¥50-200/月

### 美国区付费 API

**MovieGlu**（已弃用，改用 TMDB）:
- 需要商务洽谈
- 提供实时排片和票务信息
- 适合商业项目

---

## 🚀 快速测试

### 测试豆瓣 API（无需 key）

```bash
# 测试热门电影
curl "https://movie.douban.com/j/search_subjects?type=movie&tag=%E7%83%AD%E9%97%A8&page_limit=3" \
  -H "User-Agent: Mozilla/5.0"

# 测试搜索
curl "https://movie.douban.com/j/subject_suggest?q=%E6%B5%81%E6%B5%AA%E5%9C%B0%E7%90%83" \
  -H "User-Agent: Mozilla/5.0"
```

### 测试 TMDB API（需要 key）

```bash
# 替换为你的 TMDB API key
export TMDB_API_KEY="your_key_here"

# 测试正在热映
curl "https://api.themoviedb.org/3/movie/now_playing?api_key=$TMDB_API_KEY"

# 测试搜索
curl "https://api.themoviedb.org/3/search/movie?query=inception&api_key=$TMDB_API_KEY"
```

---

## 📈 性能对比

| 指标 | 豆瓣 | TMDB |
|------|------|------|
| 响应时间 | ~300ms | ~500ms |
| 成功率 | ~99% | ~99% |
| 数据更新频率 | 实时 | 实时 |
| 并发限制 | 较低 | 40 次/10 秒 |
| 数据完整性 | ⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ |

---

## 🎓 总结建议

### 对于学习和开发（推荐新手）

**配置**: 豆瓣 API + TMDB API

**理由**:
- ✅ 零成本（TMDB 只需免费注册）
- ✅ 数据质量高
- ✅ 足够用于学习和原型开发
- ✅ 了解基本流程

**获取步骤**:
1. 豆瓣 API：无需操作，直接使用
2. TMDB API：注册账号 → 申请 API Key → 复制使用（5 分钟）

### 对于生产环境（商业用途）

**配置**: 付费中国 API + TMDB API

**理由**:
- ✅ 数据完整（包含实时排片）
- ✅ 稳定可靠
- ✅ 有技术支持

**成本**: 约 ¥100-500/月（中国区付费 API）

### 对于 Demo 展示

**配置**: 仅使用豆瓣 API

**理由**:
- ✅ 最简单（无需注册）
- ✅ 零成本
- ⚠️ 功能有限（无实时排片）

---

## 📞 常见问题

### Q: TMDB API 真的免费吗？

A: 是的！TMDB 是非营利社区驱动的项目，API 对非商业用途完全免费。只需注册账号即可获取 API Key，无需信用卡。

### Q: TMDB 注册需要什么条件？

A: 只需要有效邮箱地址。注册后在 Settings → API 中申请，填写基本信息（用途、网站等），立即生效。

### Q: 豆瓣 API 为什么免费？

A: 这是豆瓣官方的页面接口，被豆瓣爱好者发现并广泛使用。虽然不是官方公开的 API，但豆瓣一直默许其存在。

### Q: 能否同时使用多个 API？

A: 可以！我们的代码支持配置多个 API，会自动根据城市名选择合适的提供商。

### Q: 实时排片数据从哪里获取？

A: 
- 中国大陆：建议使用付费 API（聚合数据、APISpace 等）
- 美国：TMDB 不提供，需要联系影院或使用其他商业 API

### Q: TMDB API Key 会过期吗？

A: 不会。只要不违反使用条款，API Key 永久有效。如果密钥泄露，可以在后台重新生成。

---

## 🔗 相关链接

- **豆瓣电影**: https://movie.douban.com/
- **TMDB 官网**: https://www.themoviedb.org/
- **TMDB API 文档**: https://developer.themoviedb.org/docs
- **TMDB API 申请**: https://www.themoviedb.org/settings/api
- **聚合数据（付费）**: https://www.juhe.cn/
- **APISpace（付费）**: https://www.apispace.com/

---

**最后更新**: 2026-03-07

**注意**: API 政策可能随时变化，请以官方最新公告为准。本文档仅供参考。
