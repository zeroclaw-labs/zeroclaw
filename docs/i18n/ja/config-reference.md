# 設定リファレンス（日本語）

このページは Wave 1 の初版ローカライズです。主要設定キー、既定値、リスク境界を確認します。

英語版原文:

- [../../config-reference.md](../../config-reference.md)

## 主な用途

- 新規環境の初期設定
- 設定衝突や回復手順の確認
- セキュリティ関連設定の監査

## 運用ルール

- 設定キー名は英語のまま保持します。
- 実行時挙動の定義は英語版原文を優先します。
- 追加キー: `observability.runtime_trace_record_http`（LLM の HTTP リクエスト/レスポンス詳細を `llm_http_request` / `llm_http_response` として記録）。詳細は英語版を参照してください。
