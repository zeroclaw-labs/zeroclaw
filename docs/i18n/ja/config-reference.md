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
- 追加キー: `observability.runtime_trace_record_http`（LLM の HTTP リクエスト/レスポンス詳細を `llm_http_request` / `llm_http_response` として記録）。デフォルト値 `false`、`runtime_trace_mode` が `rolling` または `full` の場合のみ有効。ペイロードは機密フィールドをマスクしますが、trace ファイルは機密運用データの扱いが必要です。リクエスト/レスポンス/ヘッダーはサイズ超過で切り捨てられます。本番環境では無効化を検討してください。詳細は英語版を参照してください。

## `[observability]`

| キー | デフォルト | 目的 |
|---|---|---|
| `backend` | `none` | 可観測性バックエンド：`none`、`noop`、`log`、`prometheus`、`otel`、`opentelemetry` または `otlp` |
| `otel_endpoint` | `http://localhost:4318` | バックエンドが `otel` の場合に使用される OTLP HTTP エンドポイント |
| `otel_service_name` | `zeroclaw` | OTLP コレクターに送信されるサービス名 |
| `runtime_trace_mode` | `none` | ランタイムトレースストレージモード：`none`、`rolling`、または `full` |
| `runtime_trace_path` | `state/runtime-trace.jsonl` | ランタイムトレース JSONL パス（絶対パスでない場合はワークスペース相対） |
| `runtime_trace_max_entries` | `200` | `runtime_trace_mode = "rolling"` の場合に保持される最大イベント数 |
| `runtime_trace_record_http` | `false` | 詳細な LLM HTTP リクエスト/レスポンスイベント（`llm_http_request` / `llm_http_response`）をランタイムトレースに記録 |

備考：

- `backend = "otel"` はブロッキングエクスポータークライアントを使用して OTLP HTTP エクスポートを行うため、非 Tokio コンテキストからも安全に span と metric を送信できます。
- エイリアス値 `opentelemetry` と `otlp` は同じ OTel バックエンドにマッピングされます。
- ランタイムトレースはツールコール失敗や不正なモデルツールペイロードのデバッグを目的としています。モデルの出力テキストが含まれる可能性があるため、共有ホストではデフォルトで無効にしてください。
- `runtime_trace_record_http` は `runtime_trace_mode` が `rolling` または `full` の場合にのみ有効です。
  - HTTP トレースペイロードは一般的な機密フィールド（例：Authorization ヘッダーや token のようなクエリ/本文フィールド）をマスクしますが、trace ファイルは機密運用データとして扱ってください。
  - ストリーミングリクエストでは、効率向上のためレスポンス本文の記録をスキップし、リクエスト本文のみを引き続き記録します（サイズ制限内）。
  - リクエスト/レスポンス/ヘッダー値は過大の場合に切り詰められます。ただし、大きなレスポンスを使用する高ボリューム LLM トラフィックは、メモリ使用量とトレースファイルサイズを急増させる可能性があります。
  - 本番環境では HTTP トレースを無効にすることを検討してください。
- ランタイムトレースを検索：
  - `zeroclaw doctor traces --limit 20`
  - `zeroclaw doctor traces --event tool_call_result --contains \"error\"`
  - `zeroclaw doctor traces --event llm_http_response --contains \"500\"`
  - `zeroclaw doctor traces --id <trace-id>`

例：

```toml
[observability]
backend = "otel"
otel_endpoint = "http://localhost:4318"
otel_service_name = "zeroclaw"
runtime_trace_mode = "rolling"
runtime_trace_path = "state/runtime-trace.jsonl"
runtime_trace_max_entries = 200
runtime_trace_record_http = true
```
