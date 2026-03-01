# Z.AI GLM 設定指南（繁體中文）

ZeroClaw 透過 OpenAI 相容端點支援 Z.AI 的 GLM 模型。
本指南涵蓋與 ZeroClaw provider 行為相符的實用設定選項。

## 概覽

ZeroClaw 內建支援以下 Z.AI 別名與端點：

| 別名 | 端點 | 備註 |
|-------|----------|-------|
| `zai` | `https://api.z.ai/api/coding/paas/v4` | 全球端點 |
| `zai-cn` | `https://open.bigmodel.cn/api/paas/v4` | 中國端點 |

若需使用自訂基礎 URL，請參閱 `docs/custom-providers.md`。

## 設定

### 快速開始

```bash
zeroclaw onboard \
  --provider "zai" \
  --api-key "YOUR_ZAI_API_KEY"
```

### 手動設定

編輯 `~/.zeroclaw/config.toml`：

```toml
api_key = "YOUR_ZAI_API_KEY"
default_provider = "zai"
default_model = "glm-5"
default_temperature = 0.7
```

## 可用模型

| 模型 | 說明 |
|-------|-------------|
| `glm-5` | 初始化引導預設模型；推理能力最強 |
| `glm-4.7` | 強大的通用品質 |
| `glm-4.6` | 均衡的基準模型 |
| `glm-4.5-air` | 較低延遲的選項 |

模型可用性因帳號/地區而異，如有疑問請使用 `/models` API 查詢。

## 驗證設定

### 使用 curl 測試

```bash
# 測試 OpenAI 相容端點
curl -X POST "https://api.z.ai/api/coding/paas/v4/chat/completions" \
  -H "Authorization: Bearer YOUR_ZAI_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "glm-5",
    "messages": [{"role": "user", "content": "Hello"}]
  }'
```

預期回應：
```json
{
  "choices": [{
    "message": {
      "content": "Hello! How can I help you today?",
      "role": "assistant"
    }
  }]
}
```

### 使用 ZeroClaw CLI 測試

```bash
# 直接測試 agent
echo "Hello" | zeroclaw agent

# 檢查狀態
zeroclaw status
```

## 環境變數

新增至 `.env` 檔案：

```bash
# Z.AI API Key
ZAI_API_KEY=your-id.secret

# 選用的通用金鑰（多數 provider 使用）
# API_KEY=your-id.secret
```

金鑰格式為 `id.secret`（例如：`abc123.xyz789`）。

## 疑難排解

### 頻率限制

**症狀：** `rate_limited` 錯誤

**解決方式：**
- 等待後重試
- 確認您的 Z.AI 方案額度限制
- 嘗試使用 `glm-4.5-air` 以獲得較低延遲與較高配額容忍度

### 驗證錯誤

**症狀：** 401 或 403 錯誤

**解決方式：**
- 確認 API key 格式為 `id.secret`
- 檢查金鑰是否已過期
- 確保金鑰中沒有多餘的空白

### 找不到模型

**症狀：** 模型不可用錯誤

**解決方式：**
- 列出可用模型：
```bash
curl -s "https://api.z.ai/api/coding/paas/v4/models" \
  -H "Authorization: Bearer YOUR_ZAI_API_KEY" | jq '.data[].id'
```

## 取得 API Key

1. 前往 [Z.AI](https://z.ai)
2. 註冊 Coding Plan
3. 從儀表板產生 API key
4. 金鑰格式：`id.secret`（例如：`abc123.xyz789`）

## 相關文件

- [ZeroClaw README](../README.md)
- [自訂 Provider 端點](./custom-providers.md)
- [貢獻指南](../CONTRIBUTING.md)
