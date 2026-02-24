# ローカライズブリッジ: Custom Providers

このページは強化版ブリッジです。テーマの位置付け、原文セクション導線、実行時の注意点をまとめています。

英語版原文:

- [../../custom-providers.md](../../custom-providers.md)

## テーマ位置付け

- 分類: Provider と統合
- 深度: 強化ブリッジ（セクション導線 + 実行ヒント）
- 使い方: 構成を把握してから、英語版の規範記述に従って実施します。

## 原文セクションガイド

- [H2 · Provider Types](../../custom-providers.md#provider-types)
- [H3 · OpenAI-Compatible Endpoints (`custom:`)](../../custom-providers.md#openai-compatible-endpoints-custom)
- [H3 · Anthropic-Compatible Endpoints (`anthropic-custom:`)](../../custom-providers.md#anthropic-compatible-endpoints-anthropic-custom)
- [H2 · Configuration Methods](../../custom-providers.md#configuration-methods)
- [H3 · Config File](../../custom-providers.md#config-file)
- [H3 · Environment Variables](../../custom-providers.md#environment-variables)
- [H2 · llama.cpp Server (Recommended Local Setup)](../../custom-providers.md#llama-cpp-server-recommended-local-setup)
- [H2 · SGLang Server](../../custom-providers.md#sglang-server)
- [H2 · vLLM Server](../../custom-providers.md#vllm-server)
- [H2 · Testing Configuration](../../custom-providers.md#testing-configuration)
- [H2 · Troubleshooting](../../custom-providers.md#troubleshooting)
- [H3 · Authentication Errors](../../custom-providers.md#authentication-errors)
- [H3 · Model Not Found](../../custom-providers.md#model-not-found)
- [H3 · Connection Issues](../../custom-providers.md#connection-issues)
- [H2 · Examples](../../custom-providers.md#examples)
- [H3 · Local LLM Server (Generic Custom Endpoint)](../../custom-providers.md#local-llm-server-generic-custom-endpoint)
- [H3 · Corporate Proxy](../../custom-providers.md#corporate-proxy)
- [H3 · Cloud Provider Gateway](../../custom-providers.md#cloud-provider-gateway)

## 実行ヒント

- まず原文の見出し構成を確認し、今回の変更範囲に直結する節から読みます。
- コマンド名、設定キー、API パス、コード識別子は英語のまま保持します。
- 仕様解釈に差分が出る場合は英語版原文を優先します。

## 関連エントリ

- [README.md](README.md)
- [SUMMARY.md](SUMMARY.md)
- [docs-inventory.md](docs-inventory.md)
