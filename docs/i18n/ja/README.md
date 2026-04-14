<p align="center">
  <img src="../../assets/quantclaw-banner.png" alt="QuantClaw" width="600" />
</p>

<h1 align="center">🦀 QuantClaw — パーソナルAIアシスタント</h1>

<p align="center">
  <strong>ゼロオーバーヘッド。ゼロ妥協。100% Rust。100% 非依存。</strong><br>
  ⚡️ <strong>10ドルのハードウェアで5MB未満のRAMで動作：OpenClawより99%少ないメモリ、Mac miniより98%安い！</strong>
</p>

<p align="center">
ハーバード大学、MIT、Sundai.Clubコミュニティの学生とメンバーにより構築。
</p>

<p align="center">
  🌐 <strong>Languages:</strong>
  <a href="../../../README.md">🇺🇸 English</a> ·
  <a href="../zh-CN/README.md">🇨🇳 简体中文</a> ·
  <a href="../ja/README.md">🇯🇵 日本語</a> ·
  <a href="../ko/README.md">🇰🇷 한국어</a> ·
  <a href="../vi/README.md">🇻🇳 Tiếng Việt</a> ·
  <a href="../tl/README.md">🇵🇭 Tagalog</a> ·
  <a href="../es/README.md">🇪🇸 Español</a> ·
  <a href="../pt/README.md">🇧🇷 Português</a> ·
  <a href="../it/README.md">🇮🇹 Italiano</a> ·
  <a href="../de/README.md">🇩🇪 Deutsch</a> ·
  <a href="../fr/README.md">🇫🇷 Français</a> ·
  <a href="../ar/README.md">🇸🇦 العربية</a> ·
  <a href="../hi/README.md">🇮🇳 हिन्दी</a> ·
  <a href="../ru/README.md">🇷🇺 Русский</a> ·
  <a href="../bn/README.md">🇧🇩 বাংলা</a> ·
  <a href="../he/README.md">🇮🇱 עברית</a> ·
  <a href="../pl/README.md">🇵🇱 Polski</a> ·
  <a href="../cs/README.md">🇨🇿 Čeština</a> ·
  <a href="../nl/README.md">🇳🇱 Nederlands</a> ·
  <a href="../tr/README.md">🇹🇷 Türkçe</a> ·
  <a href="../uk/README.md">🇺🇦 Українська</a> ·
  <a href="../id/README.md">🇮🇩 Bahasa Indonesia</a> ·
  <a href="../th/README.md">🇹🇭 ไทย</a> ·
  <a href="../ur/README.md">🇵🇰 اردو</a> ·
  <a href="../ro/README.md">🇷🇴 Română</a> ·
  <a href="../sv/README.md">🇸🇪 Svenska</a> ·
  <a href="../el/README.md">🇬🇷 Ελληνικά</a> ·
  <a href="../hu/README.md">🇭🇺 Magyar</a> ·
  <a href="../fi/README.md">🇫🇮 Suomi</a> ·
  <a href="../da/README.md">🇩🇰 Dansk</a> ·
  <a href="../nb/README.md">🇳🇴 Norsk</a>
</p>

QuantClawは、あなた自身のデバイスで実行するパーソナルAIアシスタントです。既に使用しているチャンネル（WhatsApp、Telegram、Slack、Discord、Signal、iMessage、Matrix、IRC、Email、Bluesky、Nostr、Mattermost、Nextcloud Talk、DingTalk、Lark、QQ、Reddit、LinkedIn、Twitter、MQTT、WeChat Workなど）で応答します。リアルタイム制御用のウェブダッシュボードを備え、ハードウェア周辺機器（ESP32、STM32、Arduino、Raspberry Pi）に接続できます。Gatewayはコントロールプレーンに過ぎず、製品はアシスタントそのものです。

ローカルで高速、常時稼働のパーソナルなシングルユーザーアシスタントが必要なら、これがその答えです。

<p align="center">
  <a href="https://quantspeed.ai">ウェブサイト</a> ·
  <a href="docs/README.md">ドキュメント</a> ·
  <a href="docs/architecture.md">アーキテクチャ</a> ·
  <a href="#クイックスタートtldr">はじめに</a> ·
  <a href="#openclawからの移行">OpenClawからの移行</a> ·
  <a href="docs/ops/troubleshooting.md">トラブルシューティング</a> ·
</p>

> **推奨セットアップ：** ターミナルで `quantclaw onboard` を実行してください。QuantClaw Onboardがゲートウェイ、ワークスペース、チャンネル、プロバイダーのセットアップをステップバイステップでガイドします。これは推奨されるセットアップパスで、macOS、Linux、Windows（WSL2経由）で動作します。新規インストール？ここから開始：[はじめに](#クイックスタートtldr)

### サブスクリプション認証（OAuth）

- **OpenAI Codex**（ChatGPTサブスクリプション）
- **Gemini**（Google OAuth）
- **Anthropic**（APIキーまたは認証トークン）

モデルに関する注意：多くのプロバイダー/モデルがサポートされていますが、最良のエクスペリエンスのために、利用可能な最新世代の最も強力なモデルを使用してください。[オンボーディング](#クイックスタートtldr)を参照。

モデル設定 + CLI：[プロバイダーリファレンス](docs/reference/api/providers-reference.md)
認証プロファイルローテーション（OAuth vs APIキー）+ フェイルオーバー：[モデルフェイルオーバー](docs/reference/api/providers-reference.md)

## インストール（推奨）

ランタイム：Rust stable ツールチェーン。単一バイナリ、ランタイム依存なし。

### Homebrew（macOS/Linuxbrew）

```bash
brew install quantclaw
```

### ワンクリックブートストラップ

```bash
git clone https://github.com/quant-speed/quantclaw.git
cd quantclaw
./install.sh
```

`quantclaw onboard` はインストール後に自動的に実行され、ワークスペースとプロバイダーを設定します。

## クイックスタート（TL;DR）

完全な初心者ガイド（認証、ペアリング、チャンネル）：[はじめに](docs/setup-guides/one-click-bootstrap.md)

```bash
# インストール + オンボード
./install.sh --api-key "sk-..." --provider openrouter

# ゲートウェイを起動（webhookサーバー + ウェブダッシュボード）
quantclaw gateway                # デフォルト：127.0.0.1:42617
quantclaw gateway --port 0       # ランダムポート（セキュリティ強化）

# アシスタントと会話
quantclaw agent -m "Hello, QuantClaw!"

# インタラクティブモード
quantclaw agent

# フル自律ランタイムを起動（ゲートウェイ + チャンネル + cron + hands）
quantclaw daemon

# ステータス確認
quantclaw status

# 診断を実行
quantclaw doctor
```

アップグレード？更新後に `quantclaw doctor` を実行してください。

### ソースからビルド（開発）

```bash
git clone https://github.com/quant-speed/quantclaw.git
cd quantclaw

cargo build --release --locked
cargo install --path . --force --locked

quantclaw onboard
```

> **開発用代替手段（グローバルインストールなし）：** コマンドの前に `cargo run --release --` を付けてください（例：`cargo run --release -- status`）。

## OpenClawからの移行

QuantClawはOpenClawのワークスペース、メモリ、設定をインポートできます：

```bash
# 移行内容のプレビュー（安全、読み取り専用）
quantclaw migrate openclaw --dry-run

# 移行を実行
quantclaw migrate openclaw
```

これにより、メモリエントリ、ワークスペースファイル、設定が `~/.openclaw/` から `~/.quantclaw/` に移行されます。設定はJSONからTOMLに自動変換されます。

## セキュリティデフォルト（DMアクセス）

QuantClawは実際のメッセージングサービスに接続します。着信DMを信頼できない入力として扱ってください。

完全なセキュリティガイド：[SECURITY.md](SECURITY.md)

すべてのチャンネルのデフォルト動作：

- **DMペアリング**（デフォルト）：不明な送信者には短いペアリングコードが送信され、ボットはメッセージを処理しません。
- 承認方法：`quantclaw pairing approve <channel> <code>`（送信者がローカル許可リストに追加されます）。
- パブリック着信DMには `config.toml` での明示的なオプトインが必要です。
- `quantclaw doctor` を実行してリスクのある、または設定ミスのあるDMポリシーを検出します。

**自律レベル：**

| レベル | 動作 |
|--------|------|
| `ReadOnly` | エージェントは観察のみで操作不可 |
| `Supervised`（デフォルト） | エージェントは中/高リスク操作時に承認が必要 |
| `Full` | エージェントはポリシー範囲内で自律的に操作 |

**サンドボックス層：** ワークスペース分離、パストラバーサルブロック、コマンド許可リスト、禁止パス（`/etc`、`/root`、`~/.ssh`）、レート制限（時間あたり最大アクション数、日あたりコスト上限）。

<!-- BEGIN:WHATS_NEW -->
<!-- END:WHATS_NEW -->

### 📢 お知らせ

このボードは重要な通知（破壊的変更、セキュリティアドバイザリ、メンテナンスウィンドウ、リリースブロッカー）に使用します。

| 日付 (UTC) | レベル | 通知 | 対応 |
| ---------- | ------ | ---- | ---- |
| 2026-02-19 | _重大_ | 当プロジェクトは `openagen/quantclaw`、`quantclaw.org`、`quantclaw.net` とは**一切関係ありません**。`quantclaw.org` と `quantclaw.net` ドメインは現在 `openagen/quantclaw` フォークを指しており、そのドメイン/リポジトリは当プロジェクトの公式ウェブサイト/プロジェクトを偽装しています。 | それらのソースからの情報、バイナリ、資金調達、告知を信頼しないでください。[このリポジトリ](https://github.com/quant-speed/quantclaw)と認証済みのソーシャルアカウントのみを使用してください。 |
| 2026-02-19 | _重要_ | Anthropicは2026-02-19に認証と資格情報の使用に関する規約を更新しました。Claude Code OAuthトークン（Free、Pro、Max）はClaude CodeおよびClaude.ai専用です。Claude Free/Pro/MaxのOAuthトークンを他の製品、ツール、サービス（Agent SDKを含む）で使用することは許可されておらず、消費者利用規約に違反する可能性があります。 | 潜在的な損失を防ぐため、一時的にClaude Code OAuth統合を避けてください。元の条項：[Authentication and Credential Use](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use)。 |

## ハイライト

- **デフォルトでリーンなランタイム** — 一般的なCLIとステータスワークフローは、リリースビルドで数メガバイトのメモリエンベロープで実行されます。
- **コスト効率の良いデプロイ** — 10ドルボードや小規模クラウドインスタンス向けに設計、重量級ランタイム依存なし。
- **高速コールドスタート** — シングルバイナリRustランタイムにより、コマンドとデーモンの起動がほぼ瞬時。
- **ポータブルアーキテクチャ** — ARM、x86、RISC-Vにまたがる単一バイナリで、プロバイダー/チャンネル/ツールが交換可能。
- **ローカルファーストゲートウェイ** — セッション、チャンネル、ツール、cron、SOP、イベントの単一コントロールプレーン。
- **マルチチャンネル受信箱** — WhatsApp、Telegram、Slack、Discord、Signal、iMessage、Matrix、IRC、Email、Bluesky、Nostr、Mattermost、Nextcloud Talk、DingTalk、Lark、QQ、Reddit、LinkedIn、Twitter、MQTT、WeChat Work、WebSocketなど。
- **マルチエージェントオーケストレーション（Hands）** — スケジュールに基づいて実行され、時間とともにスマートになる自律エージェントスウォーム。
- **標準運用手順（SOPs）** — MQTT、webhook、cron、周辺機器トリガーによるイベント駆動ワークフロー自動化。
- **ウェブダッシュボード** — React 19 + Viteウェブ UIで、リアルタイムチャット、メモリブラウザ、設定エディタ、cronマネージャー、ツールインスペクター。
- **ハードウェア周辺機器** — `Peripheral` traitを通じてESP32、STM32 Nucleo、Arduino、Raspberry Pi GPIOをサポート。
- **ファーストクラスツール** — shell、ファイルI/O、ブラウザ、git、ウェブフェッチ/検索、MCP、Jira、Notion、Google Workspaceなど70以上。
- **ライフサイクルフック** — あらゆる段階でLLM呼び出し、ツール実行、メッセージをインターセプトおよび変更。
- **スキルプラットフォーム** — バンドル、コミュニティ、ワークスペーススキルとセキュリティ監査。
- **トンネルサポート** — Cloudflare、Tailscale、ngrok、OpenVPN、カスタムトンネルによるリモートアクセス。

### チームがQuantClawを選ぶ理由

- **デフォルトでリーン：** 小型Rustバイナリ、高速起動、低メモリフットプリント。
- **設計によるセキュリティ：** ペアリング、厳格なサンドボックス、明示的な許可リスト、ワークスペーススコーピング。
- **完全に交換可能：** コアシステムはすべてtrait（プロバイダー、チャンネル、ツール、メモリ、トンネル）。
- **ロックインなし：** OpenAI互換プロバイダーサポート + プラガブルなカスタムエンドポイント。

## ベンチマークスナップショット（QuantClaw vs OpenClaw、再現可能）

ローカルマシンクイックベンチマーク（macOS arm64、2026年2月）、0.8GHzエッジハードウェア向けに正規化。

|                           | OpenClaw      | NanoBot        | PicoClaw        | QuantClaw 🦀          |
| ------------------------- | ------------- | -------------- | --------------- | -------------------- |
| **言語**                  | TypeScript    | Python         | Go              | **Rust**             |
| **RAM**                   | > 1GB         | > 100MB        | < 10MB          | **< 5MB**            |
| **起動時間（0.8GHzコア）** | > 500s        | > 30s          | < 1s            | **< 10ms**           |
| **バイナリサイズ**        | ~28MB (dist)  | N/A (Scripts)  | ~8MB            | **~8.8 MB**          |
| **コスト**                | Mac Mini $599 | Linux SBC ~$50 | Linux Board $10 | **任意のハードウェア $10** |

> 注意：QuantClawの結果はリリースビルドで `/usr/bin/time -l` を使用して測定されています。OpenClawにはNode.jsランタイム（通常約390MBの追加メモリオーバーヘッド）が必要で、NanoBotにはPythonランタイムが必要です。PicoClawとQuantClawは静的バイナリです。上記のRAM数値はランタイムメモリです。ビルド時のコンパイル要件はより高くなります。

<p align="center">
  <img src="docs/assets/quantclaw-comparison.jpeg" alt="QuantClaw vs OpenClaw Comparison" width="800" />
</p>

### 再現可能なローカル測定

```bash
cargo build --release
ls -lh target/release/quantclaw

/usr/bin/time -l target/release/quantclaw --help
/usr/bin/time -l target/release/quantclaw status
```

## これまでに構築したすべて

### コアプラットフォーム

- Gateway HTTP/WS/SSEコントロールプレーン：セッション、プレゼンス、設定、cron、webhook、ウェブダッシュボード、ペアリング。
- CLIサーフェス：`gateway`、`agent`、`onboard`、`doctor`、`status`、`service`、`migrate`、`auth`、`cron`、`channel`、`skills`。
- エージェントオーケストレーションループ：ツールディスパッチ、プロンプト構築、メッセージ分類、メモリロード。
- セッションモデル：セキュリティポリシー実行、自律レベル、承認ゲーティング。
- レジリエントプロバイダーラッパー：20以上のLLMバックエンドにわたるフェイルオーバー、リトライ、モデルルーティング。

### チャンネル

チャンネル：WhatsApp（ネイティブ）、Telegram、Slack、Discord、Signal、iMessage、Matrix、IRC、Email、Bluesky、DingTalk、Lark、Mattermost、Nextcloud Talk、Nostr、QQ、Reddit、LinkedIn、Twitter、MQTT、WeChat Work、WATI、Mochat、Linq、Notion、WebSocket、ClawdTalk。

フィーチャーゲート：Matrix（`channel-matrix`）、Lark（`channel-lark`）、Nostr（`channel-nostr`）。

### ウェブダッシュボード

React 19 + Vite 6 + Tailwind CSS 4 ウェブダッシュボード、Gatewayから直接提供：

- **ダッシュボード** — システム概要、ヘルスステータス、アップタイム、コストトラッキング
- **エージェントチャット** — エージェントとのインタラクティブチャット
- **メモリ** — メモリエントリの閲覧と管理
- **設定** — 設定の表示と編集
- **Cron** — スケジュールタスクの管理
- **ツール** — 利用可能なツールの閲覧
- **ログ** — エージェントアクティビティログの表示
- **コスト** — トークン使用量とコストトラッキング
- **Doctor** — システムヘルス診断
- **インテグレーション** — インテグレーションステータスとセットアップ
- **ペアリング** — デバイスペアリング管理

### ファームウェアターゲット

| ターゲット | プラットフォーム | 用途 |
|------------|------------------|------|
| ESP32 | Espressif ESP32 | ワイヤレス周辺機器エージェント |
| ESP32-UI | ESP32 + Display | ビジュアルインターフェース付きエージェント |
| STM32 Nucleo | STM32 (ARM Cortex-M) | 産業用周辺機器 |
| Arduino | Arduino | 基本センサー/アクチュエーターブリッジ |
| Uno Q Bridge | Arduino Uno | エージェントへのシリアルブリッジ |

### ツール + 自動化

- **コア：** shell、ファイル読み書き/編集、git操作、glob検索、コンテンツ検索
- **ウェブ：** ブラウザ制御、ウェブフェッチ、ウェブ検索、スクリーンショット、画像情報、PDF読み取り
- **インテグレーション：** Jira、Notion、Google Workspace、Microsoft 365、LinkedIn、Composio、Pushover
- **MCP：** Model Context Protocolツールラッパー + 遅延ツールセット
- **スケジューリング：** cron追加/削除/更新/実行、スケジュールツール
- **メモリ：** 想起、保存、忘却、知識、プロジェクトインテル
- **高度：** 委譲（エージェント間）、スウォーム、モデル切り替え/ルーティング、セキュリティオプス、クラウドオプス
- **ハードウェア：** ボード情報、メモリマップ、メモリ読み取り（フィーチャーゲート）

### ランタイム + 安全性

- **自律レベル：** ReadOnly、Supervised（デフォルト）、Full。
- **サンドボックス：** ワークスペース分離、パストラバーサルブロック、コマンド許可リスト、禁止パス、Landlock（Linux）、Bubblewrap。
- **レート制限：** 時間あたり最大アクション数、日あたり最大コスト（設定可能）。
- **承認ゲーティング：** 中/高リスク操作のインタラクティブ承認。
- **緊急停止：** 緊急シャットダウン機能。
- **129以上のセキュリティテスト** が自動化CIに含まれています。

### 運用 + パッケージング

- ウェブダッシュボードはGatewayから直接提供。
- トンネルサポート：Cloudflare、Tailscale、ngrok、OpenVPN、カスタムコマンド。
- Dockerランタイムアダプターによるコンテナ化実行。
- CI/CD：beta（プッシュ時自動）→ stable（手動ディスパッチ）→ Docker、crates.io、Scoop、AUR、Homebrew、tweet。
- プリビルドバイナリ：Linux（x86_64、aarch64、armv7）、macOS（x86_64、aarch64）、Windows（x86_64）。


## 設定

最小 `~/.quantclaw/config.toml`：

```toml
default_provider = "anthropic"
api_key = "sk-ant-..."
```

完全な設定リファレンス：[docs/reference/api/config-reference.md](docs/reference/api/config-reference.md)。

### チャンネル設定

**Telegram：**
```toml
[channels.telegram]
bot_token = "123456:ABC-DEF..."
```

**Discord：**
```toml
[channels.discord]
token = "your-bot-token"
```

**Slack：**
```toml
[channels.slack]
bot_token = "xoxb-..."
app_token = "xapp-..."
```

**WhatsApp：**
```toml
[channels.whatsapp]
enabled = true
```

**Matrix：**
```toml
[channels.matrix]
homeserver_url = "https://matrix.org"
username = "@bot:matrix.org"
password = "..."
```

**Signal：**
```toml
[channels.signal]
phone_number = "+1234567890"
```

### トンネル設定

```toml
[tunnel]
kind = "cloudflare"  # or "tailscale", "ngrok", "openvpn", "custom", "none"
```

詳細：[チャンネルリファレンス](docs/reference/api/channels-reference.md) · [設定リファレンス](docs/reference/api/config-reference.md)

### ランタイムサポート（現在）

- **`native`**（デフォルト）— 直接プロセス実行、最速パス、信頼できる環境に最適。
- **`docker`** — 完全なコンテナ分離、強制セキュリティポリシー、Docker必要。

厳格なサンドボックスまたはネットワーク分離には `runtime.kind = "docker"` を設定してください。

## サブスクリプション認証（OpenAI Codex / Claude Code / Gemini）

QuantClawはサブスクリプションネイティブ認証プロファイル（マルチアカウント、保存時暗号化）をサポートしています。

- ストアファイル：`~/.quantclaw/auth-profiles.json`
- 暗号化キー：`~/.quantclaw/.secret_key`
- プロファイルIDフォーマット：`<provider>:<profile_name>`（例：`openai-codex:work`）

```bash
# OpenAI Codex OAuth（ChatGPTサブスクリプション）
quantclaw auth login --provider openai-codex --device-code

# Gemini OAuth
quantclaw auth login --provider gemini --profile default

# Anthropic setup-token
quantclaw auth paste-token --provider anthropic --profile default --auth-kind authorization

# チェック / リフレッシュ / プロファイル切り替え
quantclaw auth status
quantclaw auth refresh --provider openai-codex --profile default
quantclaw auth use --provider openai-codex --profile work

# サブスクリプション認証でエージェントを実行
quantclaw agent --provider openai-codex -m "hello"
quantclaw agent --provider anthropic -m "hello"
```

## エージェントワークスペース + スキル

ワークスペースルート：`~/.quantclaw/workspace/`（設定で変更可能）。

注入されるプロンプトファイル：
- `IDENTITY.md` — エージェントの人格と役割
- `USER.md` — ユーザーコンテキストと好み
- `MEMORY.md` — 長期的な事実と教訓
- `AGENTS.md` — セッション規約と初期化ルール
- `SOUL.md` — コアアイデンティティと運用原則

スキル：`~/.quantclaw/workspace/skills/<skill>/SKILL.md` または `SKILL.toml`。

```bash
# インストール済みスキルの一覧
quantclaw skills list

# gitからインストール
quantclaw skills install https://github.com/user/my-skill.git

# インストール前のセキュリティ監査
quantclaw skills audit https://github.com/user/my-skill.git

# スキルの削除
quantclaw skills remove my-skill
```

## CLIコマンド

```bash
# ワークスペース管理
quantclaw onboard              # ガイド付きセットアップウィザード
quantclaw status               # デーモン/エージェントのステータス表示
quantclaw doctor               # システム診断を実行

# ゲートウェイ + デーモン
quantclaw gateway              # ゲートウェイサーバーを起動（127.0.0.1:42617）
quantclaw daemon               # フル自律ランタイムを起動

# エージェント
quantclaw agent                # インタラクティブチャットモード
quantclaw agent -m "message"   # 単一メッセージモード

# サービス管理
quantclaw service install      # OSサービスとしてインストール（launchd/systemd）
quantclaw service start|stop|restart|status

# チャンネル
quantclaw channel list         # 設定済みチャンネルの一覧
quantclaw channel doctor       # チャンネルヘルスの確認
quantclaw channel bind-telegram 123456789

# Cron + スケジューリング
quantclaw cron list            # スケジュールタスクの一覧
quantclaw cron add "*/5 * * * *" --prompt "Check system health"
quantclaw cron remove <id>

# メモリ
quantclaw memory list          # メモリエントリの一覧
quantclaw memory get <key>     # メモリの取得
quantclaw memory stats         # メモリ統計

# 認証プロファイル
quantclaw auth login --provider <name>
quantclaw auth status
quantclaw auth use --provider <name> --profile <profile>

# ハードウェア周辺機器
quantclaw hardware discover    # 接続デバイスのスキャン
quantclaw peripheral list      # 接続周辺機器の一覧
quantclaw peripheral flash     # デバイスへのファームウェア書き込み

# 移行
quantclaw migrate openclaw --dry-run
quantclaw migrate openclaw

# シェル補完
source <(quantclaw completions bash)
quantclaw completions zsh > ~/.zfunc/_quantclaw
```

完全なコマンドリファレンス：[docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)

<!-- markdownlint-disable MD001 MD024 -->

## 前提条件

<details>
<summary><strong>Windows</strong></summary>

#### 必須

1. **Visual Studio Build Tools**（MSVCリンカーとWindows SDKを提供）：

    ```powershell
    winget install Microsoft.VisualStudio.2022.BuildTools
    ```

    インストール時（またはVisual Studioインストーラーで）、**"Desktop development with C++"** ワークロードを選択してください。

2. **Rustツールチェーン：**

    ```powershell
    winget install Rustlang.Rustup
    ```

    インストール後、新しいターミナルを開いて `rustup default stable` を実行し、stableツールチェーンがアクティブであることを確認してください。

3. 両方が動作していることを**確認**：
    ```powershell
    rustc --version
    cargo --version
    ```

#### オプション

- **Docker Desktop** — [Dockerサンドボックスランタイム](#ランタイムサポート現在)（`runtime.kind = "docker"`）を使用する場合のみ必要。`winget install Docker.DockerDesktop` でインストール。

</details>

<details>
<summary><strong>Linux / macOS</strong></summary>

#### 必須

1. **ビルドツール：**
    - **Linux (Debian/Ubuntu):** `sudo apt install build-essential pkg-config`
    - **Linux (Fedora/RHEL):** `sudo dnf group install development-tools && sudo dnf install pkg-config`
    - **macOS:** Xcodeコマンドラインツールをインストール：`xcode-select --install`

2. **Rustツールチェーン：**

    ```bash
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
    ```

    詳細は [rustup.rs](https://rustup.rs) を参照。

3. 両方が動作していることを**確認**：
    ```bash
    rustc --version
    cargo --version
    ```

#### ワンラインインストーラー

または、上記のステップをスキップして、単一コマンドですべてをインストール（システム依存、Rust、QuantClaw）：

```bash
curl -LsSf https://raw.githubusercontent.com/quant-speed/quantclaw/master/install.sh | bash
```

#### コンパイルリソース要件

ソースからのビルドは、結果のバイナリを実行するよりも多くのリソースが必要です：

| リソース | 最小 | 推奨 |
| -------- | ---- | ---- |
| **RAM + swap** | 2 GB | 4 GB+ |
| **空きディスク** | 6 GB | 10 GB+ |

ホストが最小要件を下回る場合、プリビルドバイナリを使用してください：

```bash
./install.sh --prefer-prebuilt
```

ソースフォールバックなしのバイナリのみインストール：

```bash
./install.sh --prebuilt-only
```

#### オプション

- **Docker** — [Dockerサンドボックスランタイム](#ランタイムサポート現在)（`runtime.kind = "docker"`）を使用する場合のみ必要。パッケージマネージャーまたは [docker.com](https://docs.docker.com/engine/install/) からインストール。

> **注意：** デフォルトの `cargo build --release` は `codegen-units=1` を使用してコンパイルのピーク圧力を低減します。強力なマシンでのビルド高速化には `cargo build --profile release-fast` を使用してください。

</details>

<!-- markdownlint-enable MD001 MD024 -->

### プリビルドバイナリ

リリースアセットは以下で公開されています：

- Linux: `x86_64`、`aarch64`、`armv7`
- macOS: `x86_64`、`aarch64`
- Windows: `x86_64`

最新アセットはこちらからダウンロード：
<https://github.com/quant-speed/quantclaw/releases/latest>

## ドキュメント

オンボーディングフローを終えて、より深いリファレンスが必要な場合に使用してください。

- ナビゲーションと「どこに何があるか」は[ドキュメントインデックス](docs/README.md)から。
- [アーキテクチャ概要](docs/architecture.md)で完全なシステムモデルを確認。
- すべてのキーと例は[設定リファレンス](docs/reference/api/config-reference.md)で。
- [運用ランブック](docs/ops/operations-runbook.md)に従ってGatewayを実行。
- [QuantClaw Onboard](#クイックスタートtldr)でガイド付きセットアップ。
- [トラブルシューティングガイド](docs/ops/troubleshooting.md)で一般的な障害をデバッグ。
- 何かを公開する前に[セキュリティガイダンス](docs/security/README.md)を確認。

### リファレンスドキュメント

- ドキュメントハブ：[docs/README.md](docs/README.md)
- 統一ドキュメント目次：[docs/SUMMARY.md](docs/SUMMARY.md)
- コマンドリファレンス：[docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)
- 設定リファレンス：[docs/reference/api/config-reference.md](docs/reference/api/config-reference.md)
- プロバイダーリファレンス：[docs/reference/api/providers-reference.md](docs/reference/api/providers-reference.md)
- チャンネルリファレンス：[docs/reference/api/channels-reference.md](docs/reference/api/channels-reference.md)
- 運用ランブック：[docs/ops/operations-runbook.md](docs/ops/operations-runbook.md)
- トラブルシューティング：[docs/ops/troubleshooting.md](docs/ops/troubleshooting.md)

### コラボレーションドキュメント

- 貢献ガイド：[CONTRIBUTING.md](CONTRIBUTING.md)
- PRワークフローポリシー：[docs/contributing/pr-workflow.md](docs/contributing/pr-workflow.md)
- CIワークフローガイド：[docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- レビューアープレイブック：[docs/contributing/reviewer-playbook.md](docs/contributing/reviewer-playbook.md)
- セキュリティ開示ポリシー：[SECURITY.md](SECURITY.md)
- ドキュメントテンプレート：[docs/contributing/doc-template.md](docs/contributing/doc-template.md)

### デプロイ + 運用

- ネットワークデプロイガイド：[docs/ops/network-deployment.md](docs/ops/network-deployment.md)
- プロキシエージェントプレイブック：[docs/ops/proxy-agent-playbook.md](docs/ops/proxy-agent-playbook.md)
- ハードウェアガイド：[docs/hardware/README.md](docs/hardware/README.md)

## Icy Crab 🦀

QuantClawはsmooth crab 🦀のために構築されました。高速で効率的なAIアシスタント。Argenis De La Rosaとコミュニティによって構築されました。

- [quantspeed.ai](https://quantspeed.ai)
- [@quantspeed](https://x.com/quantspeed)

## QuantClawを支援

QuantClawがあなたの仕事に役立ち、継続的な開発を支援したい場合は、こちらから寄付できます：

<a href="https://buymeacoffee.com/argenistherose"><img src="https://img.shields.io/badge/Buy%20Me%20a%20Coffee-Donate-yellow.svg?style=for-the-badge&logo=buy-me-a-coffee" alt="Buy Me a Coffee" /></a>

### 🙏 特別な感謝

このオープンソースの取り組みにインスピレーションと活力を与えてくれたコミュニティと機関に心からの感謝を：

- **ハーバード大学** — 知的好奇心を育み、可能性の限界を押し広げてくれたことに感謝。
- **MIT** — オープンな知識、オープンソース、そしてテクノロジーは誰もがアクセスできるべきという信念を擁護してくれたことに感謝。
- **Sundai Club** — コミュニティ、エネルギー、そして意味のあるものを構築するための弛まぬ努力に感謝。
- **世界とその先** 🌍✨ — オープンソースを良い力にしているすべての貢献者、夢想家、構築者へ。これはあなたのためのものです。

最高のアイデアはあらゆるところから生まれるため、私たちはオープンに構築しています。これを読んでいるなら、あなたはその一部です。ようこそ。🦀❤️

## 貢献

QuantClaw初心者ですか？[`good first issue`](https://github.com/quant-speed/quantclaw/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) ラベルの付いた課題を探してください — 始め方は[貢献ガイド](CONTRIBUTING.md#first-time-contributors)を参照。AI/vibe-coded PRも歓迎します！🤖

[CONTRIBUTING.md](CONTRIBUTING.md) と [CLA.md](docs/contributing/cla.md) を参照。traitを実装してPRを提出してください：

- CIワークフローガイド：[docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- 新 `Provider` → `src/providers/`
- 新 `Channel` → `src/channels/`
- 新 `Observer` → `src/observability/`
- 新 `Tool` → `src/tools/`
- 新 `Memory` → `src/memory/`
- 新 `Tunnel` → `src/tunnel/`
- 新 `Peripheral` → `src/peripherals/`
- 新 `Skill` → `~/.quantclaw/workspace/skills/<name>/`

<!-- BEGIN:RECENT_CONTRIBUTORS -->
<!-- END:RECENT_CONTRIBUTORS -->

## ⚠️ 公式リポジトリと偽装警告

**これがQuantClawの唯一の公式リポジトリです：**

> https://github.com/quant-speed/quantclaw

「QuantClaw」を名乗る、またはQuantClaw Labsとの提携を示唆する他のリポジトリ、組織、ドメイン、パッケージは**無許可であり、本プロジェクトとは無関係です**。既知の無許可フォークは [TRADEMARK.md](docs/maintainers/trademark.md) に記載されます。

偽装や商標の悪用を見つけた場合は、[issueを作成](https://github.com/quant-speed/quantclaw/issues)してください。

---

## ライセンス

QuantClawは最大限のオープン性と貢献者保護のためにデュアルライセンスです：

| ライセンス | 用途 |
|------------|------|
| [MIT](LICENSE-MIT) | オープンソース、研究、学術、個人使用 |
| [Apache 2.0](LICENSE-APACHE) | 特許保護、機関、商用デプロイ |

どちらのライセンスでも選択できます。**貢献者は両方のライセンスの権利を自動的に付与します** — 完全な貢献者契約については [CLA.md](docs/contributing/cla.md) を参照してください。

### 商標

**QuantClaw** の名称とロゴはQuantClaw Labsの商標です。このライセンスは、推薦や提携を暗示するための使用許可を付与しません。許可された使用と禁止された使用については [TRADEMARK.md](docs/maintainers/trademark.md) を参照してください。

### 貢献者の保護

- あなたは貢献の**著作権を保持**します
- **特許付与**（Apache 2.0）により、他の貢献者からの特許請求から保護されます
- あなたの貢献はコミット履歴と [NOTICE](NOTICE) に**永続的に帰属**されます
- 貢献により商標権は移転されません

---

**QuantClaw** — ゼロオーバーヘッド。ゼロ妥協。どこでもデプロイ。何でも交換。🦀

## 貢献者

<a href="https://github.com/quant-speed/quantclaw/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=quant-speed/quantclaw" alt="QuantClaw contributors" />
</a>

このリストはGitHub貢献者グラフから生成され、自動的に更新されます。

## Star履歴

<p align="center">
  <a href="https://www.star-history.com/#quant-speed/quantclaw&type=date&legend=top-left">
    <picture>
     <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=quant-speed/quantclaw&type=date&theme=dark&legend=top-left" />
     <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=quant-speed/quantclaw&type=date&legend=top-left" />
     <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=quant-speed/quantclaw&type=date&legend=top-left" />
    </picture>
  </a>
</p>
