# トラブルシューティング（日本語）

このページは Wave 1 の初版ローカライズです。よくある障害の切り分け入口です。

英語版原文:

- [../../troubleshooting.md](../../troubleshooting.md)

## 主な用途

- インストール失敗や起動不良の対応
- `status`/`doctor` を使った段階的診断
- 最小ロールバックと復旧確認

## 運用ルール

- エラーコード・ログキー・コマンド名は英語のまま保持します。
- 詳細な障害シグネチャは英語版原文を優先します。

## 更新メモ

### `web_search_tool` で `403`/`429` エラーが発生する

**症状**：`DuckDuckGo search failed with status: 403`（または `429`）のようなエラーが表示される。

**原因**：一部のネットワーク/プロキシが DuckDuckGo HTML エンドポイントをブロックしている。

**修正オプション**：

1. Brave に切り替える：
```toml
[web_search]
enabled = true
provider = "brave"
brave_api_key = "<SECRET>"
```

2. Exa に切り替える：
```toml
[web_search]
enabled = true
provider = "exa"
api_key = "<SECRET>"
# 任意
# api_url = "https://api.exa.ai/search"
```

3. Tavily に切り替える：
```toml
[web_search]
enabled = true
provider = "tavily"
api_key = "<SECRET>"
# 任意
# api_url = "https://api.tavily.com/search"
```

4. Firecrawl に切り替える（ビルドに含まれている場合のみ）：
```toml
[web_search]
enabled = true
provider = "firecrawl"
api_key = "<SECRET>"
```

### shell tool で `curl`/`wget` がブロックされる

**症状**：`Command blocked: high-risk command is disallowed by policy` が出力される。

**原因**：`curl`/`wget` は自律性ポリシーにより高リスクコマンドとしてブロックされている。

**修正**：shell fetch の代わりに専用ツールを使用する：
- `http_request`：直接 API/HTTP 呼び出し
- `web_fetch`：ページコンテンツの取得/要約

最小設定：
```toml
[http_request]
enabled = true
allowed_domains = ["*"]

[web_fetch]
enabled = true
provider = "fast_html2md"
allowed_domains = ["*"]
```

### `web_fetch`/`http_request` — ホストが許可されていない

**症状**：`Host '<domain>' is not in http_request.allowed_domains` のようなエラーが表示される。

**修正**：対象ドメインを追加するか、公開アクセスに `"*"` を設定する：
```toml
[http_request]
enabled = true
allowed_domains = ["*"]

[web_fetch]
enabled = true
allowed_domains = ["*"]
blocked_domains = []
```

**セキュリティ注意**：`"*"` を設定してもローカル/プライベートネットワークはブロックされたままです。
