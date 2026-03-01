# Provider リファレンス（日本語）

このページは Wave 1 の初版ローカライズです。provider ID、別名、認証環境変数の確認に使います。

英語版原文:

- [../../providers-reference.md](../../providers-reference.md)

## 主な用途

- provider/モデル接続先を選定する
- provider ID・alias・認証変数を確認する
- provider 設定ミスや認証エラーを切り分ける

## 運用ルール

- Provider ID と環境変数名は英語のまま保持します。
- 正式な仕様は英語版原文を優先します。

## 更新ノート

- 2026-03-01: StepFun provider 対応を追加（`stepfun`、alias: `step` / `step-ai` / `step_ai`）。

## StepFun クイックガイド

- Provider ID: `stepfun`
- Aliases: `step`, `step-ai`, `step_ai`
- Base API URL: `https://api.stepfun.com/v1`
- Endpoints: `POST /v1/chat/completions`, `GET /v1/models`
- 認証 env var: `STEP_API_KEY`（fallback: `STEPFUN_API_KEY`）
- 既定モデル: `step-3.5-flash`

クイック検証:

```bash
export STEP_API_KEY="your-stepfun-api-key"
zeroclaw models refresh --provider stepfun
zeroclaw agent --provider stepfun --model step-3.5-flash -m "ping"
```
