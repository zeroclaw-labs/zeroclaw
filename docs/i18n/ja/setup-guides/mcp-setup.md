# MCPサーバーの登録

ZeroClawは**Model Context Protocol (MCP)**をサポートしており、外部ツールやコンテキストプロバイダーを使用してエージェントの機能を拡張できます。このガイドでは、MCPサーバーの登録と設定方法について説明します。

## 概要

MCPサーバーは、以下の3つのトランスポートタイプを介して接続できます：
- **stdio**: ローカルで実行されるプロセス（例：Node.jsやPythonスクリプト）。
- **sse**: Server-Sent Eventsを介したリモートサーバー。
- **http**: シンプルなHTTP POSTベースのサーバー。

## 設定方法

MCPサーバーは、`config.toml`の`[mcp]`セクションで設定します。

```toml
[mcp]
enabled = true
deferred_loading = true # 推奨：必要なときだけツールのスキーマを読み込む

[[mcp.servers]]
name = "my_local_tool"
transport = "stdio"
command = "node"
args = ["/path/to/server.js"]
env = { "API_KEY" = "secret_value" }

[[mcp.servers]]
name = "my_remote_tool"
transport = "sse"
url = "https://mcp.example.com/sse"
```

### サーバー設定項目

| 項目 | 型 | 説明 |
|-------|------|-------------|
| `name` | 文字列 | **必須**。ツールプレフィックスとして使用される表示名 (`name__tool_name`)。 |
| `transport` | 文字列 | `stdio`, `sse`, または `http`。デフォルトは `stdio`。 |
| `command` | 文字列 | (stdio のみ) 実行するコマンド。 |
| `args` | リスト | (stdio のみ) コマンドライン引数。 |
| `env` | マップ | (stdio のみ) 環境変数。 |
| `url` | 文字列 | (sse/http のみ) サーバーのエンドポイントURL。 |
| `headers` | マップ | (sse/http のみ) カスタムHTTPヘッダー（認証用など）。 |
| `tool_timeout_secs` | 整数 | このサーバーのツールの呼び出しごとのタイムアウト（秒）。 |

## セキュリティと自動承認

デフォルトでは、自律レベル（autonomy level）が `full` に設定されていない限り、MCPサーバーからのツールの実行には手動での承認が必要です。

特定のMCPサーバーのツールを自動的に承認するには、`[autonomy]`セクションの `auto_approve` リストにそのプレフィックスを追加します。

```toml
[autonomy]
auto_approve = [
  "my_local_tool__read_file", # 'my_local_tool' の特定ツールを許可
  "my_remote_tool__get_weather" # 'my_remote_tool' の特定ツールを許可
]
```

## ヒント

- **ツールのフィルタリング**: プロジェクト設定の `tool_filter_groups` を使用して、LLMに公開するMCPツールを制限できます。
- **遅延読み込み (Deferred Loading)**: `deferred_loading = true` に設定すると、最初はツール名のみをLLMに送信するため、トークンの消費を抑えることができます。エージェントがそのツールの使用を決定したときにのみ、完全なスキーマを取得します。
