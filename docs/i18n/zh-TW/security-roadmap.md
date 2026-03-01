# ZeroClaw 安全性改進路線圖（繁體中文）

> ⚠️ **狀態：提案 / 規劃路線**
>
> 本文件描述提案中的方法，可能包含假設性的指令或設定。
> 有關目前的執行期行為，請參閱 [config-reference.md](../../config-reference.md)、[operations-runbook.md](../../operations-runbook.md) 及 [troubleshooting.md](../../troubleshooting.md)。

## 現況：堅實的基礎

ZeroClaw 已具備**優異的應用層安全防護**：

✅ 指令允許清單（非封鎖清單）
✅ 路徑穿越防護
✅ 指令注入封鎖（`$(...)`、反引號、`&&`、`>`）
✅ 秘密隔離（API 金鑰不會洩漏至 shell）
✅ 速率限制（每小時 20 個動作）
✅ 頻道授權（空白 = 全部拒絕，`*` = 全部允許）
✅ 風險分級（低/中/高）
✅ 環境變數清理
✅ 禁止路徑封鎖
✅ 全面的測試覆蓋（1,017 個測試）

## 缺少的部分：作業系統層級隔離

🔴 無作業系統層級沙箱（chroot、容器、命名空間）
🔴 無資源限制（CPU、記憶體、磁碟 I/O 上限）
🔴 無防篡改稽核日誌
🔴 無系統呼叫過濾（seccomp）

---

## 比較：ZeroClaw vs PicoClaw vs 正式環境等級

| 功能 | PicoClaw | 目前的 ZeroClaw | ZeroClaw + 路線圖 | 正式環境目標 |
|------|----------|----------------|-------------------|-------------|
| **二進位大小** | ~8MB | **3.4MB** ✅ | 3.5-4MB | < 5MB |
| **記憶體使用** | < 10MB | **< 5MB** ✅ | < 10MB | < 20MB |
| **啟動時間** | < 1s | **< 10ms** ✅ | < 50ms | < 100ms |
| **指令允許清單** | 不明 | ✅ 有 | ✅ 有 | ✅ 有 |
| **路徑封鎖** | 不明 | ✅ 有 | ✅ 有 | ✅ 有 |
| **注入防護** | 不明 | ✅ 有 | ✅ 有 | ✅ 有 |
| **作業系統沙箱** | 無 | ❌ 無 | ✅ Firejail/Landlock | ✅ 容器/命名空間 |
| **資源限制** | 無 | ❌ 無 | ✅ cgroups/監控 | ✅ 完整 cgroups |
| **稽核日誌** | 無 | ❌ 無 | ✅ HMAC 簽章 | ✅ SIEM 整合 |
| **安全評分** | C | **B+** | **A-** | **A+** |

---

## 實作路線圖

### 第一階段：快速達成（1-2 週）
**目標**：以最小複雜度解決關鍵缺口

| 任務 | 檔案 | 工作量 | 影響 |
|------|------|--------|------|
| Landlock 檔案系統沙箱 | `src/security/landlock.rs` | 2 天 | 高 |
| 記憶體監控 + OOM 終止 | `src/resources/memory.rs` | 1 天 | 高 |
| 每指令 CPU 逾時 | `src/tools/shell.rs` | 1 天 | 高 |
| 基礎稽核日誌 | `src/security/audit.rs` | 2 天 | 中 |
| 設定架構更新 | `src/config/schema.rs` | 1 天 | - |

**交付項目**：
- Linux：檔案系統存取限制於工作區
- 全平台：記憶體/CPU 防護，防止失控指令
- 全平台：防篡改稽核軌跡

---

### 第二階段：平台整合（2-3 週）
**目標**：深度作業系統整合，達到正式環境級隔離

| 任務 | 工作量 | 影響 |
|------|--------|------|
| Firejail 自動偵測 + 包裝 | 3 天 | 非常高 |
| macOS/*nix 的 Bubblewrap 包裝器 | 4 天 | 非常高 |
| cgroups v2 systemd 整合 | 3 天 | 高 |
| seccomp 系統呼叫過濾 | 5 天 | 高 |
| 稽核日誌查詢 CLI | 2 天 | 中 |

**交付項目**：
- Linux：透過 Firejail 實現類容器級隔離
- macOS：Bubblewrap 檔案系統隔離
- Linux：cgroups 資源限制強制執行
- Linux：系統呼叫允許清單

---

### 第三階段：正式環境強化（1-2 週）
**目標**：企業級安全功能

| 任務 | 工作量 | 影響 |
|------|--------|------|
| Docker 沙箱模式選項 | 3 天 | 高 |
| 頻道憑證固定 | 2 天 | 中 |
| 簽章式設定檔驗證 | 2 天 | 中 |
| SIEM 相容稽核匯出 | 2 天 | 中 |
| 安全自檢（`zeroclaw audit --check`） | 1 天 | 低 |

**交付項目**：
- 選用的 Docker 執行隔離
- 頻道 webhook 的 HTTPS 憑證固定
- 設定檔簽章驗證
- JSON/CSV 稽核匯出供外部分析

---

## 新設定架構預覽

```toml
[security]
level = "strict"  # relaxed | default | strict | paranoid

# 沙箱設定
[security.sandbox]
enabled = true
backend = "auto"  # auto | firejail | bubblewrap | landlock | docker | none

# 資源限制
[resources]
max_memory_mb = 512
max_memory_per_command_mb = 128
max_cpu_percent = 50
max_cpu_time_seconds = 60
max_subprocesses = 10

# 稽核日誌
[security.audit]
enabled = true
log_path = "~/.config/zeroclaw/audit.log"
sign_events = true
max_size_mb = 100

# 自主性（既有功能，已增強）
[autonomy]
level = "supervised"  # readonly | supervised | full
allowed_commands = ["git", "ls", "cat", "grep", "find"]
forbidden_paths = ["/etc", "/root", "~/.ssh"]
require_approval_for_medium_risk = true
block_high_risk_commands = true
max_actions_per_hour = 20
```

---

## CLI 指令預覽

```bash
# 安全狀態檢查
zeroclaw security --check
# → ✓ Sandbox: Firejail active
# → ✓ Audit logging enabled (42 events today)
# → → Resource limits: 512MB mem, 50% CPU

# 稽核日誌查詢
zeroclaw audit --user @alice --since 24h
zeroclaw audit --risk high --violations-only
zeroclaw audit --verify-signatures

# 沙箱測試
zeroclaw sandbox --test
# → Testing isolation...
#   ✓ Cannot read /etc/passwd
#   ✓ Cannot access ~/.ssh
#   ✓ Can read /workspace
```

---

## 總結

**ZeroClaw 已經比 PicoClaw 更安全**，具備：
- 小 50% 的二進位檔（3.4MB vs 8MB）
- 少 50% 的記憶體用量（< 5MB vs < 10MB）
- 快 100 倍的啟動速度（< 10ms vs < 1s）
- 全面的安全策略引擎
- 廣泛的測試覆蓋

**實施本路線圖後**，ZeroClaw 將具備：
- 正式環境等級的作業系統層級沙箱
- 具記憶體/CPU 防護的資源感知能力
- 含防篡改日誌的稽核能力
- 可設定安全等級的企業就緒能力

**預估工作量**：完整實作約 4-7 週
**價值**：將 ZeroClaw 從「適合測試」提升為「適合正式環境」
