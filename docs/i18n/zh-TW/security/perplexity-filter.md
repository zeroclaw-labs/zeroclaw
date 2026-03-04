# 困惑度過濾器（選擇啟用）

ZeroClaw 提供一個選擇啟用的輕量級統計過濾器，可在訊息傳送給 LLM 提供者之前偵測對抗性後綴（例如 GCG 風格的最佳化亂碼尾部）。

## 範圍

- 適用於頻道與 gateway 入站訊息，在提供者執行之前生效。
- 不需要外部模型呼叫或重量級防護模型。
- 預設為停用，以確保相容性與延遲可預測性。

## 運作方式

過濾器使用以下方式評估提示詞的尾端視窗：

1. 字元類別二元組困惑度。
2. 後綴標點符號比例。
3. GCG 風格 token 模式檢查（標點符號 + 字母 + 數字混合）。

僅當異常標準被滿足時，訊息才會被封鎖。

## 組態

```toml
[security.perplexity_filter]
enable_perplexity_filter = true
perplexity_threshold = 16.5
suffix_window_chars = 72
min_prompt_chars = 40
symbol_ratio_threshold = 0.25
```

## 延遲

實作的時間複雜度為 O(n)（相對於提示詞長度），且不涉及網路呼叫。
本機除錯安全的回歸測試包含一個嚴格的 `<50ms` 預算測試，適用於典型的多句提示詞負載。

## 調校指引

- 如果出現誤判，請提高 `perplexity_threshold`。
- 提高 `symbol_ratio_threshold` 以減少對技術字串的封鎖。
- 提高 `min_prompt_chars` 以忽略統計資料較弱的短提示詞。
- 除非您明確需要這層額外的防禦，否則請保持此功能停用。
