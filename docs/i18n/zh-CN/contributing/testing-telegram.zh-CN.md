# 🧪 测试执行指南

## 快速参考

```bash
# 完整自动化测试套件（约 2 分钟）
./tests/telegram/test_telegram_integration.sh

# 快速冒烟测试（约 10 秒）
./tests/telegram/quick_test.sh

# 仅编译和单元测试（约 30 秒）
cargo test telegram --lib
```

## 📝 已为你创建的内容

### 1. **test_telegram_integration.sh**（主测试套件）

- **20+ 自动化测试** 覆盖所有修复
- **6 个测试阶段**：代码质量、构建、配置、健康检查、功能、手动
- **彩色输出** 带通过/失败指示器
- 结尾提供 **详细摘要**

```bash
./tests/telegram/test_telegram_integration.sh
```

### 2. **quick_test.sh**（快速验证）

- **4 个核心测试** 用于快速反馈
- **<10 秒** 执行时间
- 完美适合 **pre-commit** 检查

```bash
./tests/telegram/quick_test.sh
```

### 3. **generate_test_messages.py**（测试助手）

- 生成各种长度的测试消息
- 测试消息拆分功能
- 8 种不同的消息类型

```bash
# 生成一条长消息（>4096 字符）
python3 tests/telegram/generate_test_messages.py long

# 显示所有消息类型
python3 tests/telegram/generate_test_messages.py all
```

### 4. **TESTING_TELEGRAM.md**（完整指南）

- 全面的测试文档
- 故障排除指南
- 性能基准
- CI/CD 集成示例

## 🚀 分步指南：首次运行

### 步骤 1：运行自动化测试

```bash
cd /Users/abdzsam/jhedaiclaw

# 赋予脚本执行权限（已完成）
chmod +x tests/telegram/test_telegram_integration.sh tests/telegram/quick_test.sh

# 运行完整测试套件
./tests/telegram/test_telegram_integration.sh
```

**预期输出：**

```
⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡

███████╗███████╗██████╗  ██████╗  ██████╗██╗      █████╗ ██╗    ██╗
...

🧪 TELEGRAM INTEGRATION TEST SUITE 🧪

Phase 1: Code Quality Tests
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Test 1: Compiling test suite
✓ PASS: Test suite compiles successfully

Test 2: Running Telegram unit tests
✓ PASS: All Telegram unit tests passed (24 tests)
...

Test Summary
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Total Tests:   20
Passed:        20
Failed:        0
Warnings:      0

Pass Rate:     100%

✓ ALL AUTOMATED TESTS PASSED! 🎉
```

### 步骤 2：配置 Telegram（如果未完成）

```bash
# 交互式设置
jhedaiclaw onboard

# 或仅渠道设置
jhedaiclaw onboard --channels-only
```

提示时：

1. 选择 **Telegram** 渠道
2. 输入从 @BotFather 获取的 **机器人令牌**
3. 输入你的 **Telegram 用户 ID** 或用户名

### 步骤 3：验证健康状态

```bash
jhedaiclaw channel doctor
```

**预期输出：**

```
🩺 JhedaiClaw Channel Doctor

  ✅ Telegram  healthy

Summary: 1 healthy, 0 unhealthy, 0 timed out
```

### 步骤 4：手动测试

#### 测试 1：基础消息

```bash
# 终端 1：启动渠道
jhedaiclaw channel start
```

**在 Telegram 中：**

- 找到你的机器人
- 发送：`Hello bot!`
- **验证：** 机器人在 3 秒内响应

#### 测试 2：长消息（拆分测试）

```bash
# 生成一条长消息
python3 tests/telegram/generate_test_messages.py long
```

- **复制输出**
- **粘贴到 Telegram** 发送给你的机器人
- **验证：**
  - 消息被拆分为 2+ 个块
  - 第一个块以 `(continues...)` 结尾
  - 中间块带有 `(continued)` 和 `(continues...)`
  - 最后一个块以 `(continued)` 开头
  - 所有块按顺序到达

#### 测试 3：单词边界拆分

```bash
python3 tests/telegram/generate_test_messages.py word
```

- 发送给机器人
- **验证：** 在单词边界拆分（不会拆分单词中间）

## 🎯 测试结果检查清单

运行所有测试后，验证：

### 自动化测试

- [ ] ✅ 所有 20 个自动化测试通过
- [ ] ✅ 构建成功完成
- [ ] ✅ 二进制大小 <10MB
- [ ] ✅ 健康检查在 <5 秒内完成
- [ ] ✅ 无 clippy 警告

### 手动测试

- [ ] ✅ 机器人响应基础消息
- [ ] ✅ 长消息正确拆分
- [ ] ✅ 出现继续标记
- [ ] ✅ 尊重单词边界
- [ ] ✅ 白名单阻止未授权用户
- [ ] ✅ 日志中无错误

### 性能

- [ ] ✅ 响应时间 <3 秒
- [ ] ✅ 内存使用 <10MB
- [ ] ✅ 无消息丢失
- [ ] ✅ 速率限制正常工作（100ms 延迟）

## 🐛 故障排除

### 问题：测试编译失败

```bash
# 清理构建
cargo clean
cargo build --release

# 更新依赖
cargo update
```

### 问题："Bot token not configured"

```bash
# 检查配置
cat ~/.jhedaiclaw/config.toml | grep -A 5 telegram

# 重新配置
jhedaiclaw onboard --channels-only
```

### 问题：健康检查失败

```bash
# 直接测试机器人令牌
curl "https://api.telegram.org/bot<YOUR_TOKEN>/getMe"

# 应返回：{"ok":true,"result":{...}}
```

### 问题：机器人不响应

```bash
# 启用调试日志
RUST_LOG=debug jhedaiclaw channel start

# 查找：
# - "Telegram channel listening for messages..."
# - "ignoring message from unauthorized user"（如果是白名单问题）
# - 任何错误消息
```

## 📊 性能基准

所有修复完成后，你应该看到：

| 指标           | 目标   | 命令                               |
| -------------- | ------ | ---------------------------------- |
| 单元测试通过率 | 24/24  | `cargo test telegram --lib`        |
| 构建时间       | <30s   | `time cargo build --release`       |
| 二进制大小     | ~3-4MB | `ls -lh target/release/jhedaiclaw` |
| 健康检查       | <5s    | `time jhedaiclaw channel doctor`   |
| 首次响应       | <3s    | Telegram 中手动测试                |
| 消息拆分       | <50ms  | 检查调试日志                       |
| 内存使用       | <10MB  | `ps aux \| grep jhedaiclaw`        |

## 🔄 CI/CD 集成

添加到你的工作流：

```bash
# Pre-commit 钩子
#!/bin/bash
./tests/telegram/quick_test.sh

# CI 流水线
./tests/telegram/test_telegram_integration.sh
```

## 📚 下一步

1. **运行测试：**

   ```bash
   ./tests/telegram/test_telegram_integration.sh
   ```

2. **使用故障排除指南** 修复任何失败

3. **使用检查清单** 完成手动测试

4. **所有测试通过后** 部署到生产环境

5. **监控日志** 查看任何问题：
   ```bash
   jhedaiclaw daemon
   # 或
   RUST_LOG=info jhedaiclaw channel start
   ```

## 🎉 成功

如果所有测试通过：

- ✅ 消息拆分正常工作（4096 字符限制）
- ✅ 健康检查有 5 秒超时
- ✅ 空 chat_id 被安全处理
- ✅ 所有 24 个单元测试通过
- ✅ 代码已准备好生产环境

**你的 Telegram 集成已就绪！** 🚀

---

## 📞 支持

- Issue：<https://github.com/jhedai/jhedaiclaw/issues>
- 文档：[testing-telegram.md](../../../../tests/telegram/testing-telegram.md)
- 帮助：`jhedaiclaw --help`
