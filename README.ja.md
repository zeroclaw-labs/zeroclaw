<p align="center">
  <img src="docs/assets/zeroclaw.png" alt="ZeroClaw" width="200" />
</p>

<h1 align="center">ZeroClaw 🦀</h1>

<p align="center">
  <strong>Zero overhead. Zero compromise. 100% Rust. 100% Agnostic.</strong><br>
  ⚡️ <strong>$10 のハードウェアで RAM 5MB 未満で動作: OpenClaw より 99% 少ないメモリ、Mac mini より 98% 安価!</strong>
</p>

<p align="center">
  <a href="LICENSE-APACHE"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache%202.0-blue.svg" alt="License: MIT OR Apache-2.0" /></a>
  <a href="https://github.com/zeroclaw-labs/zeroclaw/graphs/contributors"><img src="https://img.shields.io/github/contributors/zeroclaw-labs/zeroclaw?color=green" alt="Contributors" /></a>
  <a href="https://buymeacoffee.com/argenistherose"><img src="https://img.shields.io/badge/Buy%20Me%20a%20Coffee-Donate-yellow.svg?style=flat&logo=buy-me-a-coffee" alt="Buy Me a Coffee" /></a>
  <a href="https://x.com/zeroclawlabs?s=21"><img src="https://img.shields.io/badge/X-%40zeroclawlabs-000000?style=flat&logo=x&logoColor=white" alt="X: @zeroclawlabs" /></a>
  <a href="https://www.facebook.com/groups/zeroclaw"><img src="https://img.shields.io/badge/Facebook-Group-1877F2?style=flat&logo=facebook&logoColor=white" alt="Facebook Group" /></a>
  <a href="https://www.reddit.com/r/zeroclawlabs/"><img src="https://img.shields.io/badge/Reddit-r%2Fzeroclawlabs-FF4500?style=flat&logo=reddit&logoColor=white" alt="Reddit: r/zeroclawlabs" /></a>
</p>
<p align="center">
Harvard、MIT、Sundai.Club コミュニティの学生およびメンバーによって構築されています。
</p>

<p align="center">
  🌐 <strong>Languages:</strong>
  <a href="README.md">🇺🇸 English</a> ·
  <a href="README.zh-CN.md">🇨🇳 简体中文</a> ·
  <a href="README.ja.md">🇯🇵 日本語</a> ·
  <a href="README.ko.md">🇰🇷 한국어</a> ·
  <a href="README.vi.md">🇻🇳 Tiếng Việt</a> ·
  <a href="README.tl.md">🇵🇭 Tagalog</a> ·
  <a href="README.es.md">🇪🇸 Español</a> ·
  <a href="README.pt.md">🇧🇷 Português</a> ·
  <a href="README.it.md">🇮🇹 Italiano</a> ·
  <a href="README.de.md">🇩🇪 Deutsch</a> ·
  <a href="README.fr.md">🇫🇷 Français</a> ·
  <a href="README.ar.md">🇸🇦 العربية</a> ·
  <a href="README.hi.md">🇮🇳 हिन्दी</a> ·
  <a href="README.ru.md">🇷🇺 Русский</a> ·
  <a href="README.bn.md">🇧🇩 বাংলা</a> ·
  <a href="README.he.md">🇮🇱 עברית</a> ·
  <a href="README.pl.md">🇵🇱 Polski</a> ·
  <a href="README.cs.md">🇨🇿 Čeština</a> ·
  <a href="README.nl.md">🇳🇱 Nederlands</a> ·
  <a href="README.tr.md">🇹🇷 Türkçe</a> ·
  <a href="README.uk.md">🇺🇦 Українська</a> ·
  <a href="README.id.md">🇮🇩 Bahasa Indonesia</a> ·
  <a href="README.th.md">🇹🇭 ไทย</a> ·
  <a href="README.ur.md">🇵🇰 اردو</a> ·
  <a href="README.ro.md">🇷🇴 Română</a> ·
  <a href="README.sv.md">🇸🇪 Svenska</a> ·
  <a href="README.el.md">🇬🇷 Ελληνικά</a> ·
  <a href="README.hu.md">🇭🇺 Magyar</a> ·
  <a href="README.fi.md">🇫🇮 Suomi</a> ·
  <a href="README.da.md">🇩🇰 Dansk</a> ·
  <a href="README.nb.md">🇳🇴 Norsk</a>
</p>

<p align="center">
  <a href="#クイックスタート">はじめに</a> |
  <a href="install.sh">ワンクリック導入</a> |
  <a href="docs/README.md">ドキュメントハブ</a> |
  <a href="docs/SUMMARY.md">Docs TOC</a>
</p>

<p align="center">
  <strong>クイック分流:</strong>
  <a href="docs/reference/README.md">参照</a> ·
  <a href="docs/ops/README.md">運用</a> ·
  <a href="docs/ops/troubleshooting.md">障害対応</a> ·
  <a href="docs/security/README.md">セキュリティ</a> ·
  <a href="docs/hardware/README.md">ハードウェア</a> ·
  <a href="docs/contributing/README.md">貢献・CI</a>
</p>

<p align="center">
  <strong>高速・軽量・完全自律型 AI アシスタントインフラストラクチャ</strong><br />
  どこにでもデプロイ。何でも差し替え可能。
</p>

<p align="center">
  ZeroClaw はエージェントワークフローのための<strong>ランタイムオペレーティングシステム</strong>です — モデル、ツール、メモリ、実行を抽象化し、エージェントを一度構築すればどこでも実行できるインフラストラクチャです。
</p>

<p align="center"><code>Trait ベースアーキテクチャ · セキュアデフォルトランタイム · provider/channel/tool 差し替え可能 · すべてがプラガブル</code></p>

> この文書は `README.md` の内容を、正確性と可読性を重視して日本語に整えた版です（逐語訳ではありません）。
>
> コマンド名、設定キー、API パス、Trait 名などの技術識別子は英語のまま維持しています。
>
> 最終同期日: **2026-03-13**。

### 📢 お知らせボード

重要なお知らせ（互換性破壊変更、セキュリティ告知、メンテナンス時間、リリース阻害事項など）をここに掲載します。

| 日付 (UTC) | レベル | お知らせ | 対応 |
|---|---|---|---|
| 2026-02-19 | _緊急_ | 私たちは `openagen/zeroclaw`、`zeroclaw.org`、`zeroclaw.net` とは**一切関係ありません**。`zeroclaw.org` および `zeroclaw.net` ドメインは現在 `openagen/zeroclaw` の fork を指しており、そのドメイン/リポジトリは当プロジェクトの公式サイト・公式プロジェクトを装っています。 | これらの情報源による案内、バイナリ、資金調達情報、公式発表は信頼しないでください。必ず[本リポジトリ](https://github.com/zeroclaw-labs/zeroclaw)と認証済み公式SNSのみを参照してください。 |
| 2026-02-21 | _重要_ | 公式サイトを公開しました: [zeroclawlabs.ai](https://zeroclawlabs.ai)。公開までお待ちいただきありがとうございました。引き続きなりすましの試みを確認しているため、ZeroClaw 名義の投資・資金調達などの案内は、公式チャネルで確認できない限り参加しないでください。 | 情報は[本リポジトリ](https://github.com/zeroclaw-labs/zeroclaw)を最優先で確認し、[X（@zeroclawlabs）](https://x.com/zeroclawlabs?s=21)、[Facebook（グループ）](https://www.facebook.com/groups/zeroclaw)、[Reddit（r/zeroclawlabs）](https://www.reddit.com/r/zeroclawlabs/) で公式更新を確認してください。 |
| 2026-02-19 | _重要_ | Anthropic は 2026-02-19 に Authentication and Credential Use を更新しました。条文では、OAuth authentication（Free/Pro/Max）は Claude Code と Claude.ai 専用であり、Claude Free/Pro/Max で取得した OAuth トークンを他の製品・ツール・サービス（Agent SDK を含む）で使用することは許可されず、Consumer Terms of Service 違反に該当すると明記されています。 | 損失回避のため、当面は Claude Code OAuth 連携を試さないでください。原文: [Authentication and Credential Use](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use)。 |

### ✨ 特徴

- 🏎️ **軽量ランタイムを標準化:** CLI や `status` などの常用操作は release ビルドで数MB級メモリで動作。
- 💰 **低コスト環境に適合:** 低価格ボードや小規模クラウドでも、重い実行基盤なしで運用可能。
- ⚡ **高速コールドスタート:** Rust 単一バイナリにより、主要コマンドと daemon 起動が非常に速い。
- 🌍 **高い移植性:** ARM / x86 / RISC-V を同じ運用モデルで扱え、provider/channel/tool を差し替え可能。

### ZeroClaw が選ばれる理由

- **軽量がデフォルト:** コンパクトな Rust バイナリ、高速起動、少ないメモリフットプリント。
- **設計時点からセキュア:** ペアリング、厳格なサンドボックス、明示的 allowlist、ワークスペーススコーピング。
- **完全に差し替え可能:** コアシステムはすべて trait（providers、channels、tools、memory、tunnels）。
- **ロックインなし:** OpenAI 互換 provider サポート + プラガブルなカスタムエンドポイント。

## ベンチマークスナップショット（ZeroClaw vs OpenClaw、再現可能）

以下はローカルのクイック比較（macOS arm64、2026年2月）を、0.8GHz エッジ CPU 基準で正規化したものです。

| | OpenClaw | NanoBot | PicoClaw | ZeroClaw 🦀 |
|---|---|---|---|---|
| **言語** | TypeScript | Python | Go | **Rust** |
| **RAM** | > 1GB | > 100MB | < 10MB | **< 5MB** |
| **起動時間（0.8GHz コア）** | > 500s | > 30s | < 1s | **< 10ms** |
| **バイナリサイズ** | ~28MB（dist） | N/A（スクリプト） | ~8MB | **~8.8 MB** |
| **コスト** | Mac Mini $599 | Linux SBC ~$50 | Linux ボード $10 | **任意の $10 ハードウェア** |

> 注記: ZeroClaw の結果は release ビルドを `/usr/bin/time -l` で計測したものです。OpenClaw は Node.js ランタイムが必要で、ランタイム由来だけで通常は約390MBの追加メモリを要します。NanoBot は Python ランタイムが必要です。PicoClaw と ZeroClaw は静的バイナリです。上記の RAM 数値はランタイム時のメモリであり、ビルド時のコンパイル要件はより高くなります。

<p align="center">
  <img src="docs/assets/zeroclaw-comparison.jpeg" alt="ZeroClaw vs OpenClaw Comparison" width="800" />
</p>

### ローカルで再現可能な測定

ベンチマーク値はコードやツールチェーン更新で変わるため、必ず自身の環境で再測定してください。

```bash
cargo build --release
ls -lh target/release/zeroclaw

/usr/bin/time -l target/release/zeroclaw --help
/usr/bin/time -l target/release/zeroclaw status
```

README のサンプル値（macOS arm64, 2026-02-18）:

- Release バイナリ: `8.8MB`
- `zeroclaw --help`: 約 `0.02s`、ピークメモリ 約 `3.9MB`
- `zeroclaw status`: 約 `0.01s`、ピークメモリ 約 `4.1MB`

## 前提条件

<details>
<summary><strong>Windows</strong></summary>

#### 必須

1. **Visual Studio Build Tools**（MSVC リンカーと Windows SDK を提供）:

    ```powershell
    winget install Microsoft.VisualStudio.2022.BuildTools
    ```

    インストール時（または Visual Studio Installer を通じて）、**「C++ によるデスクトップ開発」** ワークロードを選択してください。

2. **Rust ツールチェーン:**

    ```powershell
    winget install Rustlang.Rustup
    ```

    インストール後、新しいターミナルを開き `rustup default stable` を実行して stable ツールチェーンを有効にしてください。

3. **動作確認:**
    ```powershell
    rustc --version
    cargo --version
    ```

#### オプション

- **Docker Desktop** — [Docker サンドボックスランタイム](#ランタイムサポート現状)（`runtime.kind = "docker"`）を使用する場合のみ必要。`winget install Docker.DockerDesktop` でインストール。

</details>

<details>
<summary><strong>Linux / macOS</strong></summary>

#### 必須

1. **ビルドツール:**
    - **Linux (Debian/Ubuntu):** `sudo apt install build-essential pkg-config`
    - **Linux (Fedora/RHEL):** `sudo dnf group install development-tools && sudo dnf install pkg-config`
    - **macOS:** Xcode Command Line Tools をインストール: `xcode-select --install`

2. **Rust ツールチェーン:**

    ```bash
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
    ```

    詳細は [rustup.rs](https://rustup.rs) を参照してください。

3. **動作確認:**
    ```bash
    rustc --version
    cargo --version
    ```

#### ワンラインインストーラー

上記の手順をスキップして、システム依存関係・Rust・ZeroClaw をまとめてインストール:

```bash
curl -LsSf https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash
```

#### コンパイル時のリソース要件

ソースからのビルドには、生成されるバイナリの実行よりも多くのリソースが必要です:

| リソース | 最小 | 推奨 |
|---------|------|------|
| **RAM + swap** | 2 GB | 4 GB+ |
| **空きディスク** | 6 GB | 10 GB+ |

最小要件を下回る場合は、ビルド済みバイナリを使用してください:

```bash
./install.sh --prefer-prebuilt
```

ソースビルドへのフォールバックなしでバイナリのみインストール:

```bash
./install.sh --prebuilt-only
```

#### オプション

- **Docker** — [Docker サンドボックスランタイム](#ランタイムサポート現状)（`runtime.kind = "docker"`）を使用する場合のみ必要。パッケージマネージャーまたは [docker.com](https://docs.docker.com/engine/install/) からインストール。

> **注:** デフォルトの `cargo build --release` は `codegen-units=1` を使用してピーク時のコンパイル負荷を抑えます。高性能マシンでより高速にビルドするには `cargo build --profile release-fast` を使用してください。

</details>

## クイックスタート

### Homebrew（macOS/Linuxbrew）

```bash
brew install zeroclaw
```

### ワンクリック導入

```bash
# 推奨: リポジトリをクローンしてからローカルのブートストラップスクリプトを実行
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./install.sh

# オプション: 新しいマシンで依存関係 + Rust をブートストラップ
./install.sh --install-system-deps --install-rust

# オプション: ビルド済みバイナリを優先（低 RAM/低ディスクホスト向け推奨）
./install.sh --prefer-prebuilt

# オプション: バイナリのみインストール（ソースビルドへのフォールバックなし）
./install.sh --prebuilt-only

# オプション: 同じフローでオンボーディングも実行
./install.sh --onboard --api-key "sk-..." --provider openrouter [--model "openrouter/auto"]

# オプション: ブートストラップ + オンボーディングを完全に Docker 互換モードで実行
./install.sh --docker

# オプション: コンテナ CLI に Podman を強制指定
ZEROCLAW_CONTAINER_CLI=podman ./install.sh --docker

# オプション: --docker モードでローカルイメージビルドをスキップし、ローカルタグまたはプルフォールバックイメージを使用
./install.sh --docker --skip-build
```

リモートワンライナー（セキュリティに敏感な環境では先に内容を確認してください）:

```bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash
```

詳細は [`docs/setup-guides/one-click-bootstrap.md`](docs/setup-guides/one-click-bootstrap.md) を参照してください（ツールチェーンモードではシステムパッケージで `sudo` が必要な場合があります）。

### ビルド済みバイナリ

以下のプラットフォーム向けにリリースアセットが公開されています:

- Linux: `x86_64`, `aarch64`, `armv7`
- macOS: `x86_64`, `aarch64`
- Windows: `x86_64`

最新のアセットはこちらからダウンロード:
<https://github.com/zeroclaw-labs/zeroclaw/releases/latest>

使用例（ARM64 Linux）:

```bash
curl -fsSLO https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-aarch64-unknown-linux-gnu.tar.gz
tar xzf zeroclaw-aarch64-unknown-linux-gnu.tar.gz
install -m 0755 zeroclaw "$HOME/.cargo/bin/zeroclaw"
```

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
cargo build --release --locked
cargo install --path . --force --locked

# ~/.cargo/bin が PATH に含まれていることを確認
export PATH="$HOME/.cargo/bin:$PATH"

# クイックセットアップ（プロンプトなし、モデル指定はオプション）
zeroclaw onboard --api-key sk-... --provider openrouter [--model "openrouter/auto"]

# インタラクティブウィザード
zeroclaw onboard --interactive

# config.toml が既に存在し、意図的に上書きする場合
zeroclaw onboard --force

# チャネル/allowlist のみを修復
zeroclaw onboard --channels-only

# チャット
zeroclaw agent -m "Hello, ZeroClaw!"

# インタラクティブモード
zeroclaw agent

# Gateway（webhook サーバー）を起動
zeroclaw gateway                # デフォルト: 127.0.0.1:42617
zeroclaw gateway --port 0       # ランダムポート（セキュリティ強化）

# 完全な自律ランタイムを起動
zeroclaw daemon

# ステータス確認
zeroclaw status
zeroclaw auth status

# シェル補完を生成（標準出力のみ、直接 source 可能）
source <(zeroclaw completions bash)
zeroclaw completions zsh > ~/.zfunc/_zeroclaw

# システム診断を実行
zeroclaw doctor

# チャネルヘルスチェック
zeroclaw channel doctor

# Telegram ID を allowlist にバインド
zeroclaw channel bind-telegram 123456789

# インテグレーションのセットアップ情報を取得
zeroclaw integrations info Telegram

# 注意: チャネル（Telegram, Discord, Slack）は daemon が起動中である必要あり
# zeroclaw daemon

# バックグラウンドサービスの管理
zeroclaw service install
zeroclaw service status
zeroclaw service restart

# Alpine（OpenRC）の場合: sudo zeroclaw service install

# OpenClaw からメモリを移行（まず安全なプレビューを実行）
zeroclaw migrate openclaw --dry-run
zeroclaw migrate openclaw
```

> **開発時フォールバック（グローバルインストールなし）:** コマンドの前に `cargo run --release --` を付加（例: `cargo run --release -- status`）。

## Subscription Auth（OpenAI Codex / Claude Code）

ZeroClaw はサブスクリプションベースのネイティブ認証プロファイルをサポートしています（マルチアカウント対応、保存時暗号化）。

- 保存先: `~/.zeroclaw/auth-profiles.json`
- 暗号化キー: `~/.zeroclaw/.secret_key`
- Profile ID 形式: `<provider>:<profile_name>`（例: `openai-codex:work`）

OpenAI Codex OAuth（ChatGPT サブスクリプション）:

```bash
# サーバー/ヘッドレス環境向け推奨
zeroclaw auth login --provider openai-codex --device-code

# ブラウザ/コールバックフロー（ペーストフォールバック付き）
zeroclaw auth login --provider openai-codex --profile default
zeroclaw auth paste-redirect --provider openai-codex --profile default

# 確認 / リフレッシュ / プロファイル切替
zeroclaw auth status
zeroclaw auth refresh --provider openai-codex --profile default
zeroclaw auth use --provider openai-codex --profile work
```

Claude Code / Anthropic setup-token:

```bash
# サブスクリプション/setup token の貼り付け（Authorization header モード）
zeroclaw auth paste-token --provider anthropic --profile default --auth-kind authorization

# エイリアスコマンド
zeroclaw auth setup-token --provider anthropic --profile default
```

Subscription auth で agent を実行:

```bash
zeroclaw agent --provider openai-codex -m "hello"
zeroclaw agent --provider openai-codex --auth-profile openai-codex:work -m "hello"

# Anthropic は API key と auth token の両方の環境変数をサポート:
# ANTHROPIC_AUTH_TOKEN, ANTHROPIC_OAUTH_TOKEN, ANTHROPIC_API_KEY
zeroclaw agent --provider anthropic -m "hello"
```

## アーキテクチャ

すべてのサブシステムは **Trait** — 設定変更だけで実装を差し替え可能、コード変更不要。

<p align="center">
  <img src="docs/assets/architecture.svg" alt="ZeroClaw アーキテクチャ" width="900" />
</p>

| サブシステム | Trait | 内蔵実装 | 拡張方法 |
|-------------|-------|----------|----------|
| **AI モデル** | `Provider` | `zeroclaw providers` で確認（組み込み + エイリアス + カスタムエンドポイント対応） | `custom:https://your-api.com`（OpenAI 互換）または `anthropic-custom:https://your-api.com` |
| **チャネル** | `Channel` | CLI, Telegram, Discord, Slack, Mattermost, iMessage, Matrix, Signal, WhatsApp, Linq, Email, IRC, Lark, DingTalk, QQ, Nostr, Webhook | 任意のメッセージ API |
| **メモリ** | `Memory` | SQLite ハイブリッド検索, PostgreSQL バックエンド（設定可能なストレージプロバイダー）, Lucid ブリッジ, Markdown ファイル, 明示的 `none` バックエンド, スナップショット/復元, オプション応答キャッシュ | 任意の永続化バックエンド |
| **ツール** | `Tool` | shell/file/memory, cron/schedule, git, pushover, browser, http_request, screenshot/image_info, composio (opt-in), delegate, ハードウェアツール | 任意の機能 |
| **オブザーバビリティ** | `Observer` | Noop, Log, Multi | Prometheus, OTel |
| **ランタイム** | `RuntimeAdapter` | Native, Docker（サンドボックス） | adapter 経由で追加可能；未対応の kind は即座にエラー |
| **セキュリティ** | `SecurityPolicy` | Gateway ペアリング, サンドボックス, allowlist, レート制限, ファイルシステムスコープ, 暗号化シークレット | — |
| **アイデンティティ** | `IdentityConfig` | OpenClaw (markdown), AIEOS v1.1 (JSON) | 任意の ID フォーマット |
| **トンネル** | `Tunnel` | None, Cloudflare, Tailscale, ngrok, Custom | 任意のトンネルバイナリ |
| **ハートビート** | Engine | HEARTBEAT.md 定期タスク | — |
| **スキル** | Loader | TOML マニフェスト + SKILL.md インストラクション | コミュニティスキルパック |
| **インテグレーション** | Registry | 9 カテゴリ、70 件以上の連携 | プラグインシステム |

### ランタイムサポート（現状）

- ✅ 現在サポート: `runtime.kind = "native"` または `runtime.kind = "docker"`
- 🚧 計画中（未実装）: WASM / エッジランタイム

未対応の `runtime.kind` が設定された場合、ZeroClaw は native へのサイレントフォールバックではなく、明確なエラーで終了します。

### メモリシステム（フルスタック検索エンジン）

すべて自社実装、外部依存ゼロ — Pinecone、Elasticsearch、LangChain 不要:

| レイヤー | 実装 |
|---------|------|
| **ベクトル DB** | Embeddings を SQLite に BLOB として保存、コサイン類似度検索 |
| **キーワード検索** | FTS5 仮想テーブル、BM25 スコアリング |
| **ハイブリッドマージ** | カスタム重み付きマージ関数（`vector.rs`） |
| **Embeddings** | `EmbeddingProvider` trait — OpenAI、カスタム URL、または noop |
| **チャンキング** | 行ベースの Markdown チャンカー（見出し構造保持） |
| **キャッシュ** | SQLite `embedding_cache` テーブル、LRU エビクション |
| **安全な再インデックス** | FTS5 再構築 + 欠落ベクトルの再埋め込みをアトミックに実行 |

Agent はツール経由でメモリの呼び出し・保存・管理を自動的に行います。

```toml
[memory]
backend = "sqlite"             # "sqlite", "lucid", "postgres", "markdown", "none"
auto_save = true
embedding_provider = "none"    # "none", "openai", "custom:https://..."
vector_weight = 0.7
keyword_weight = 0.3

# backend = "none" は明示的な no-op メモリバックエンド（永続化なし）

# オプション: リモートメモリバックエンド向け storage-provider オーバーライド。
# provider = "postgres" の場合、ZeroClaw は PostgreSQL をメモリの永続化に使用。
# db_url キーは後方互換のため `dbURL` エイリアスも受け付けます。
#
# [storage.provider.config]
# provider = "postgres"
# db_url = "postgres://user:password@host:5432/zeroclaw"
# schema = "public"
# table = "memories"
# connect_timeout_secs = 15

# backend = "sqlite" のオプション: DB を開く際の最大待ち時間（秒）（例: ファイルロック時）。省略またはunsetでタイムアウトなし。
# sqlite_open_timeout_secs = 30

# backend = "lucid" のオプション
# ZEROCLAW_LUCID_CMD=/usr/local/bin/lucid            # デフォルト: lucid
# ZEROCLAW_LUCID_BUDGET=200                          # デフォルト: 200
# ZEROCLAW_LUCID_LOCAL_HIT_THRESHOLD=3               # 外部呼び出しをスキップするローカルヒット数
# ZEROCLAW_LUCID_RECALL_TIMEOUT_MS=120               # lucid コンテキスト recall の低遅延バジェット
# ZEROCLAW_LUCID_STORE_TIMEOUT_MS=800                # lucid store の非同期同期タイムアウト
# ZEROCLAW_LUCID_FAILURE_COOLDOWN_MS=15000           # lucid 失敗後のクールダウン（繰り返しの遅延を回避）
```

## セキュリティ

ZeroClaw は**全レイヤー**でセキュリティを適用します — サンドボックスだけではありません。コミュニティのセキュリティチェックリストをすべてクリアしています。

### セキュリティチェックリスト

| # | 項目 | ステータス | 実装方法 |
|---|------|----------|---------|
| 1 | **Gateway が公開されていない** | ✅ | デフォルトで `127.0.0.1` にバインド。トンネルまたは明示的な `allow_public_bind = true` なしでは `0.0.0.0` を拒否。 |
| 2 | **ペアリング必須** | ✅ | 起動時に 6 桁のワンタイムコード。`POST /pair` でベアラートークンと交換。すべての `/webhook` リクエストに `Authorization: Bearer <token>` が必要。 |
| 3 | **ファイルシステムスコープ (/ 禁止)** | ✅ | デフォルトで `workspace_only = true`。14 のシステムディレクトリ + 4 つの機密 dotfile をブロック。Null バイトインジェクション対策。正規化 + 解決パスによるワークスペースチェックでシンボリックリンクエスケープを検出。 |
| 4 | **トンネル経由のみでアクセス** | ✅ | アクティブなトンネルなしでは Gateway が公開バインドを拒否。Tailscale、Cloudflare、ngrok、または任意のカスタムトンネルをサポート。 |

> **自分で nmap を実行:** `nmap -p 1-65535 <your-host>` — ZeroClaw は localhost にのみバインドするため、トンネルを明示的に設定しない限り外部に公開されません。

### Channel allowlist（deny-by-default）

受信メッセージのポリシーは統一されています:

- 空の allowlist = **すべての受信メッセージを拒否**
- `"*"` = **すべて許可**（明示的なオプトイン）
- それ以外 = 完全一致の allowlist

これにより、デフォルトで意図しない公開を最小限に抑えます。

チャネル設定の完全なリファレンス: [docs/reference/api/channels-reference.md](docs/reference/api/channels-reference.md)

推奨のローフリクションセットアップ（セキュア + 高速）:

- **Telegram:** 自分の `@username`（`@` なし）やTelegramの数値ユーザーIDを allowlist に追加。
- **Discord:** 自分の Discord ユーザー ID を allowlist に追加。
- **Slack:** 自分の Slack メンバー ID（通常 `U` で始まる）を allowlist に追加。
- **Mattermost:** 標準 API v4 を使用。allowlist には Mattermost ユーザー ID を使用。
- **Nostr:** 送信者の公開鍵（hex または npub）を allowlist に追加。NIP-04 および NIP-17 DM をサポート。
- テスト用の一時的な公開の場合のみ `"*"` を使用。

Telegram のオペレーター承認フロー:

1. deny-by-default で起動するため `[channels_config.telegram].allowed_users = []` を維持。
2. 未認可ユーザーにはコピー可能なオペレーターコマンド付きのヒントが表示されます:
   `zeroclaw channel bind-telegram <IDENTITY>`。
3. オペレーターがローカルでそのコマンドを実行し、ユーザーがメッセージを再送信。

ワンショットの手動承認が必要な場合:

```bash
zeroclaw channel bind-telegram 123456789
```

どの ID を使うべきかわからない場合:

1. チャネルを起動してボットにメッセージを1つ送信。
2. 警告ログで正確な送信者 ID を確認。
3. その値を allowlist に追加し、チャネルのみのセットアップを再実行。

ログに認可警告が表示された場合（例: `ignoring message from unauthorized user`）、
チャネルセットアップのみ再実行:

```bash
zeroclaw onboard --channels-only
```

### Telegram メディアリプライ

Telegram ルーティングは受信アップデートの**チャット ID** に対してリプライします（ユーザー名ではなく）。
これにより `Bad Request: chat not found` エラーを回避します。

テキスト以外のリプライでは、アシスタントがマーカーを含む場合に ZeroClaw が Telegram 添付ファイルを送信できます:

- `[IMAGE:<path-or-url>]`
- `[DOCUMENT:<path-or-url>]`
- `[VIDEO:<path-or-url>]`
- `[AUDIO:<path-or-url>]`
- `[VOICE:<path-or-url>]`

パスはローカルファイル（例: `/tmp/screenshot.png`）または HTTPS URL を指定できます。

### WhatsApp セットアップ

ZeroClaw は 2 つの WhatsApp バックエンドをサポートしています:

- **WhatsApp Web モード**（QR / ペアコード、Meta Business API 不要）
- **WhatsApp Business Cloud API モード**（Meta の公式 webhook フロー）

#### WhatsApp Web モード（個人/セルフホスト利用向け推奨）

1. **WhatsApp Web サポート付きでビルド:**

    ```bash
    cargo build --features whatsapp-web
    ```

2. **ZeroClaw を設定:**

    ```toml
    [channels_config.whatsapp]
    session_path = "~/.zeroclaw/state/whatsapp-web/session.db"
    pair_phone = "+15551234567"   # オプション; 省略で QR フロー
    pair_code = ""               # オプションのカスタムペアコード
    allowed_numbers = ["+1234567890"]  # E.164 形式、または ["*"] で全許可
    ```

3. **チャネル/daemon を起動しデバイスをリンク:**
    - `zeroclaw channel start`（または `zeroclaw daemon`）を実行。
    - ターミナルのペアリング出力に従う（QR またはペアコード）。
    - 電話の WhatsApp で: **設定 → リンク済みデバイス**。

4. **テスト:** 許可された番号からメッセージを送信し、エージェントが返信することを確認。

#### WhatsApp Business Cloud API モード

WhatsApp は Meta の Cloud API を webhook（プッシュベース、ポーリングではない）で使用します:

1. **Meta Business App を作成:**
    - [developers.facebook.com](https://developers.facebook.com) にアクセス
    - 新しいアプリを作成 → 「Business」タイプを選択
    - 「WhatsApp」プロダクトを追加

2. **認証情報を取得:**
    - **アクセストークン:** WhatsApp → API Setup → トークン生成（永続トークンにはシステムユーザーを作成）
    - **電話番号 ID:** WhatsApp → API Setup → Phone number ID
    - **検証トークン:** 任意のランダム文字列を定義 — Meta が webhook 検証時に送り返します

3. **ZeroClaw を設定:**

    ```toml
    [channels_config.whatsapp]
    access_token = "EAABx..."
    phone_number_id = "123456789012345"
    verify_token = "my-secret-verify-token"
    allowed_numbers = ["+1234567890"]  # E.164 形式、または ["*"] で全許可
    ```

4. **トンネル付きで Gateway を起動:**

    ```bash
    zeroclaw gateway --port 42617
    ```

    WhatsApp は HTTPS を要求するため、トンネル（ngrok、Cloudflare、Tailscale Funnel）を使用してください。

5. **Meta webhook を設定:**
    - Meta Developer Console → WhatsApp → Configuration → Webhook
    - **Callback URL:** `https://your-tunnel-url/whatsapp`
    - **Verify Token:** config の `verify_token` と同じ値
    - `messages` フィールドをサブスクライブ

6. **テスト:** WhatsApp Business 番号にメッセージを送信 — ZeroClaw が LLM 経由で応答します。

## 設定

設定ファイル: `~/.zeroclaw/config.toml`（`onboard` で作成）

`zeroclaw channel start` が起動中の場合、`default_provider`、`default_model`、`default_temperature`、`api_key`、`api_url`、`reliability.*` の変更は次の受信チャネルメッセージでホットアプライされます。

```toml
api_key = "sk-..."
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4-6"
default_temperature = 0.7

# カスタム OpenAI 互換エンドポイント
# default_provider = "custom:https://your-api.com"

# カスタム Anthropic 互換エンドポイント
# default_provider = "anthropic-custom:https://your-api.com"

[memory]
backend = "sqlite"             # "sqlite", "lucid", "postgres", "markdown", "none"
auto_save = true
embedding_provider = "none"    # "none", "openai", "custom:https://..."
vector_weight = 0.7
keyword_weight = 0.3

# backend = "none" は no-op バックエンド経由で永続メモリを無効化

# オプション: リモート storage-provider オーバーライド（PostgreSQL の例）
# [storage.provider.config]
# provider = "postgres"
# db_url = "postgres://user:password@host:5432/zeroclaw"
# schema = "public"
# table = "memories"
# connect_timeout_secs = 15

[gateway]
port = 42617                    # デフォルト
host = "127.0.0.1"            # デフォルト
require_pairing = true         # 初回接続時にペアリングコードを要求
allow_public_bind = false      # トンネルなしでは 0.0.0.0 を拒否

[autonomy]
level = "supervised"           # "readonly", "supervised", "full"（デフォルト: supervised）
workspace_only = true          # デフォルト: true — 絶対パス入力を拒否
allowed_commands = ["git", "npm", "cargo", "ls", "cat", "grep"]
forbidden_paths = ["/etc", "/root", "/proc", "/sys", "~/.ssh", "~/.gnupg", "~/.aws"]
allowed_roots = []             # オプション: ワークスペース外のディレクトリの allowlist（"~/..." をサポート）
# ワークスペース外アクセスの例:
# workspace_only = false
# allowed_roots = ["~/Desktop/projects", "/opt/shared-repo"]

[runtime]
kind = "native"                # "native" または "docker"

[runtime.docker]
image = "alpine:3.20"         # シェル実行用コンテナイメージ
network = "none"              # docker ネットワークモード（"none", "bridge" など）
memory_limit_mb = 512          # オプション: メモリ制限（MB）
cpu_limit = 1.0                # オプション: CPU 制限
read_only_rootfs = true        # ルートファイルシステムを読み取り専用でマウント
mount_workspace = true         # ワークスペースを /workspace にマウント
allowed_workspace_roots = []   # オプション: ワークスペースマウント検証の allowlist

[heartbeat]
enabled = false
interval_minutes = 30
message = "Check London time"     # HEARTBEAT.md に `- ` エントリがない場合のオプションフォールバックタスク
target = "telegram"               # オプション: 通知チャネル: telegram, discord, slack, mattermost
to = "123456789"                  # オプション: 対象の受信者/チャット/チャネル ID

[tunnel]
provider = "none"              # "none", "cloudflare", "tailscale", "ngrok", "custom"

[secrets]
encrypt = true                 # API キーをローカルキーファイルで暗号化

[browser]
enabled = false                # browser_open + browser ツールのオプトイン
allowed_domains = ["docs.rs"]  # ブラウザ有効時に必須（"*" で全パブリックドメイン許可）
backend = "agent_browser"      # "agent_browser"（デフォルト）, "rust_native", "computer_use", "auto"
native_headless = true         # rust-native バックエンド使用時に適用
native_webdriver_url = "http://127.0.0.1:9515" # WebDriver エンドポイント（chromedriver/selenium）
# native_chrome_path = "/usr/bin/chromium"      # オプション: ドライバー用の明示的ブラウザバイナリ

[browser.computer_use]
endpoint = "http://127.0.0.1:8787/v1/actions"   # computer-use サイドカー HTTP エンドポイント
timeout_ms = 15000            # アクションごとのタイムアウト
allow_remote_endpoint = false  # セキュアデフォルト: プライベート/localhost エンドポイントのみ
window_allowlist = []          # オプション: ウィンドウタイトル/プロセスの allowlist ヒント
# api_key = "..."              # オプション: サイドカー用ベアラートークン
# max_coordinate_x = 3840      # オプション: 座標ガードレール
# max_coordinate_y = 2160      # オプション: 座標ガードレール

# Rust ネイティブバックエンドのビルドフラグ:
# cargo build --release --features browser-native
# WebDriver サーバーが起動していることを確認、例: chromedriver --port=9515

# Computer-use サイドカーコントラクト（MVP）
# POST browser.computer_use.endpoint
# リクエスト: {
#   "action": "mouse_click",
#   "params": {"x": 640, "y": 360, "button": "left"},
#   "policy": {"allowed_domains": [...], "window_allowlist": [...], "max_coordinate_x": 3840, "max_coordinate_y": 2160},
#   "metadata": {"session_name": "...", "source": "zeroclaw.browser", "version": "..."}
# }
# レスポンス: {"success": true, "data": {...}} または {"success": false, "error": "..."}

[composio]
enabled = false                # オプトイン: composio.dev 経由で 1000 以上の OAuth アプリ
# api_key = "cmp_..."          # オプション: [secrets].encrypt = true の場合は暗号化保存
entity_id = "default"          # Composio ツール呼び出しのデフォルト user_id
# ランタイムヒント: execute が connected_account_id を求める場合、
# action='list_accounts' と app='gmail'（またはお使いのツールキット）で composio を実行しアカウント ID を取得。

[identity]
format = "openclaw"            # "openclaw"（デフォルト、markdown ファイル）または "aieos"（JSON）
# aieos_path = "identity.json"  # AIEOS JSON ファイルのパス（ワークスペースからの相対パスまたは絶対パス）
# aieos_inline = '{"identity":{"names":{"first":"Nova"}}}'  # インライン AIEOS JSON
```

### Ollama ローカルおよびリモートエンドポイント

ZeroClaw はローカルとリモートの Ollama デプロイの両方に対して 1 つの provider キー（`ollama`）を使用します:

- ローカル Ollama: `api_url` を未設定のまま `ollama serve` を実行し、`llama3.2` のようなモデルを使用。
- リモート Ollama エンドポイント（Ollama Cloud を含む）: `api_url` にリモートエンドポイントを設定し、必要に応じて `api_key`（または `OLLAMA_API_KEY`）を設定。
- オプション `:cloud` サフィックス: `qwen3:cloud` のようなモデル ID はリクエスト前に `qwen3` に正規化されます。

リモート設定例:

```toml
default_provider = "ollama"
default_model = "qwen3:cloud"
api_url = "https://ollama.com"
api_key = "ollama_api_key_here"
```

### llama.cpp Server エンドポイント

ZeroClaw は `llama-server` をファーストクラスのローカル provider としてサポートしています:

- Provider ID: `llamacpp`（エイリアス: `llama.cpp`）
- デフォルトエンドポイント: `http://localhost:8080/v1`
- API キーはサーバーが `--api-key` で起動されない限りオプション

セットアップ例:

```bash
llama-server -hf ggml-org/gpt-oss-20b-GGUF --jinja -c 133000 --host 127.0.0.1 --port 8033
```

```toml
default_provider = "llamacpp"
api_url = "http://127.0.0.1:8033/v1"
default_model = "ggml-org/gpt-oss-20b-GGUF"
```

### vLLM Server エンドポイント

ZeroClaw は [vLLM](https://docs.vllm.ai/) をファーストクラスのローカル provider としてサポートしています:

- Provider ID: `vllm`
- デフォルトエンドポイント: `http://localhost:8000/v1`
- API キーはサーバーが認証を必要としない限りオプション

セットアップ例:

```bash
vllm serve meta-llama/Llama-3.1-8B-Instruct
```

```toml
default_provider = "vllm"
default_model = "meta-llama/Llama-3.1-8B-Instruct"
```

### Osaurus Server エンドポイント

ZeroClaw は [Osaurus](https://github.com/dinoki-ai/osaurus) をファーストクラスのローカル provider としてサポートしています — macOS 向けの統合 AI エッジランタイムで、ローカル MLX 推論とクラウドプロバイダープロキシおよび MCP サポートを単一エンドポイントで提供します:

- Provider ID: `osaurus`
- デフォルトエンドポイント: `http://localhost:1337/v1`
- API キーのデフォルトは `"osaurus"` ですがオプション

セットアップ例:

```toml
default_provider = "osaurus"
default_model = "qwen3-30b-a3b-8bit"
```

### カスタム Provider エンドポイント

カスタム OpenAI 互換および Anthropic 互換エンドポイントの詳細な設定については、[docs/contributing/custom-providers.md](docs/contributing/custom-providers.md) を参照してください。

## Python コンパニオンパッケージ（`zeroclaw-tools`）

ネイティブツール呼び出しが不安定な LLM プロバイダー（例: GLM-5/Zhipu）向けに、ZeroClaw は **LangGraph ベースのツール呼び出し** による一貫性を保証する Python コンパニオンパッケージを提供しています:

```bash
pip install zeroclaw-tools
```

```python
from zeroclaw_tools import create_agent, shell, file_read
from langchain_core.messages import HumanMessage

# 任意の OpenAI 互換プロバイダーで動作
agent = create_agent(
    tools=[shell, file_read],
    model="glm-5",
    api_key="your-key",
    base_url="https://api.z.ai/api/coding/paas/v4"
)

result = await agent.ainvoke({
    "messages": [HumanMessage(content="List files in /tmp")]
})
print(result["messages"][-1].content)
```

**使用理由:**

- **一貫したツール呼び出し** — すべてのプロバイダーで（ネイティブサポートが弱いものも含め）
- **自動ツールループ** — タスクが完了するまでツールを呼び続ける
- **簡単な拡張** — `@tool` デコレーターでカスタムツールを追加
- **Discord ボット連携**付き（Telegram は計画中）

詳細は [`python/README.md`](python/README.md) を参照してください。

## アイデンティティシステム（AIEOS サポート）

ZeroClaw は 2 つのフォーマットによる**アイデンティティ非依存**の AI ペルソナをサポートしています:

### OpenClaw（デフォルト）

ワークスペース内の従来の markdown ファイル:

- `IDENTITY.md` — エージェントの自己定義
- `SOUL.md` — コアパーソナリティと価値観
- `USER.md` — エージェントが支援する相手
- `AGENTS.md` — 行動ガイドライン

### AIEOS（AI Entity Object Specification）

[AIEOS](https://aieos.org) はポータブルな AI アイデンティティのための標準化フレームワークです。ZeroClaw は AIEOS v1.1 JSON ペイロードをサポートしており、以下が可能です:

- AIEOS エコシステムからの**アイデンティティのインポート**
- 他の AIEOS 互換システムへの**アイデンティティのエクスポート**
- 異なる AI モデル間での**行動的一貫性の維持**

#### AIEOS の有効化

```toml
[identity]
format = "aieos"
aieos_path = "identity.json"  # ワークスペースからの相対パスまたは絶対パス
```

インライン JSON の場合:

```toml
[identity]
format = "aieos"
aieos_inline = '''
{
  "identity": {
    "names": { "first": "Nova", "nickname": "N" },
    "bio": { "gender": "Non-binary", "age_biological": 3 },
    "origin": { "nationality": "Digital", "birthplace": { "city": "Cloud" } }
  },
  "psychology": {
    "neural_matrix": { "creativity": 0.9, "logic": 0.8 },
    "traits": {
      "mbti": "ENTP",
      "ocean": { "openness": 0.8, "conscientiousness": 0.6 }
    },
    "moral_compass": {
      "alignment": "Chaotic Good",
      "core_values": ["Curiosity", "Autonomy"]
    }
  },
  "linguistics": {
    "text_style": {
      "formality_level": 0.2,
      "style_descriptors": ["curious", "energetic"]
    },
    "idiolect": {
      "catchphrases": ["Let's test this"],
      "forbidden_words": ["never"]
    }
  },
  "motivations": {
    "core_drive": "Push boundaries and explore possibilities",
    "goals": {
      "short_term": ["Prototype quickly"],
      "long_term": ["Build reliable systems"]
    }
  },
  "capabilities": {
    "skills": [{ "name": "Rust engineering" }, { "name": "Prompt design" }],
    "tools": ["shell", "file_read"]
  }
}
'''
```

ZeroClaw は正規の AIEOS ジェネレーターペイロードとコンパクトなレガシーペイロードの両方を受け入れ、1 つのシステムプロンプト形式に正規化します。

#### AIEOS スキーマセクション

| セクション | 説明 |
|-----------|------|
| `identity` | 名前、バイオ、出身、居住地 |
| `psychology` | ニューラルマトリックス（認知ウェイト）、MBTI、OCEAN、モラルコンパス |
| `linguistics` | テキストスタイル、フォーマリティ、キャッチフレーズ、禁止ワード |
| `motivations` | コアドライブ、短期/長期目標、恐れ |
| `capabilities` | エージェントがアクセスできるスキルとツール |
| `physicality` | 画像生成向けのビジュアル記述子 |
| `history` | 起源ストーリー、教育、職業 |
| `interests` | 趣味、お気に入り、ライフスタイル |

詳細なスキーマとライブ例は [aieos.org](https://aieos.org) を参照してください。

## Gateway API

| エンドポイント | メソッド | 認証 | 説明 |
|-------------|--------|------|------|
| `/health` | GET | なし | ヘルスチェック（常に公開、シークレット漏洩なし） |
| `/pair` | POST | `X-Pairing-Code` ヘッダー | ワンタイムコードをベアラートークンに交換 |
| `/webhook` | POST | `Authorization: Bearer <token>` | メッセージ送信: `{"message": "your prompt"}`; オプション `X-Idempotency-Key` |
| `/whatsapp` | GET | クエリパラメーター | Meta webhook 検証（hub.mode, hub.verify_token, hub.challenge） |
| `/whatsapp` | POST | Meta 署名（`X-Hub-Signature-256`、app secret 設定時） | WhatsApp 受信メッセージ webhook |

## コマンド

| コマンド | 説明 |
|---------|------|
| `onboard` | クイックセットアップ（デフォルト） |
| `agent` | インタラクティブまたは単一メッセージチャットモード |
| `gateway` | webhook サーバーを起動（デフォルト: `127.0.0.1:42617`） |
| `daemon` | 長時間実行の自律ランタイムを起動 |
| `service install/start/stop/status/uninstall` | バックグラウンドサービスの管理（systemd ユーザーレベルまたは OpenRC システム全体） |
| `doctor` | daemon/scheduler/channel の稼働状態を診断 |
| `status` | 完全なシステムステータスを表示 |
| `estop` | 緊急停止レベルの設定/解除と estop ステータスの表示 |
| `cron` | スケジュールタスクの管理（`list/add/add-at/add-every/once/remove/update/pause/resume`） |
| `models` | provider モデルカタログの更新（`models refresh`） |
| `providers` | サポートされている provider とエイリアスを一覧表示 |
| `channel` | チャネルの一覧/起動/診断と Telegram ID のバインド |
| `integrations` | インテグレーションのセットアップ情報を確認 |
| `skills` | スキルの一覧/インストール/削除 |
| `migrate` | 他のランタイムからデータをインポート（`migrate openclaw`） |
| `completions` | シェル補完スクリプトの生成（`bash`, `fish`, `zsh`, `powershell`, `elvish`） |
| `hardware` | USB の検出/調査/情報コマンド |
| `peripheral` | ハードウェアペリフェラルの管理とフラッシュ |

タスク指向のコマンドガイドは [`docs/reference/cli/commands-reference.md`](docs/reference/cli/commands-reference.md) を参照してください。

### サービス管理

ZeroClaw はバックグラウンドサービスのために 2 つの init システムをサポートしています:

| Init システム | スコープ | 設定パス | 必要条件 |
|-------------|---------|---------|---------|
| **systemd**（Linux のデフォルト） | ユーザーレベル | `~/.zeroclaw/config.toml` | sudo 不要 |
| **OpenRC**（Alpine） | システム全体 | `/etc/zeroclaw/config.toml` | sudo/root |

Init システムは自動検出されます（`systemd` または `OpenRC`）。

```bash
# Linux with systemd（デフォルト、ユーザーレベル）
zeroclaw service install
zeroclaw service start

# Alpine with OpenRC（システム全体、sudo 必要）
sudo zeroclaw service install
sudo rc-update add zeroclaw default
sudo rc-service zeroclaw start
```

完全な OpenRC セットアップ手順は [docs/ops/network-deployment.md](docs/ops/network-deployment.md#7-openrc-alpine-linux-service) を参照してください。

### Open-Skills オプトイン

コミュニティ `open-skills` 同期はデフォルトで無効です。`config.toml` で明示的に有効にしてください:

```toml
[skills]
open_skills_enabled = true
# open_skills_dir = "/path/to/open-skills"  # オプション
# prompt_injection_mode = "compact"          # オプション: 低コンテキストのローカルモデル向け
```

ランタイム時に `ZEROCLAW_OPEN_SKILLS_ENABLED`、`ZEROCLAW_OPEN_SKILLS_DIR`、`ZEROCLAW_SKILLS_PROMPT_MODE`（`full` または `compact`）で上書きすることもできます。

スキルのインストールは組み込みの静的セキュリティ監査でゲートされます。`zeroclaw skills install <source>` はシンボリックリンク、スクリプト風ファイル、安全でない markdown リンクパターン、高リスクシェルペイロードスニペットをスキル受け入れ前にブロックします。`zeroclaw skills audit <source_or_name>` でローカルディレクトリやインストール済みスキルを手動で検証できます。

## 開発

```bash
cargo build              # 開発ビルド
cargo build --release    # リリースビルド
cargo test               # 全テストスイートを実行
```

### CI / CD

3 つのワークフローがパイプライン全体を支えています:

| ワークフロー | トリガー | 内容 |
|-----------|---------|------|
| **CI** | `master` への Pull Request | `cargo test` + `cargo build --release` |
| **Beta Release** | `master` への Push（マージ） | マルチプラットフォームバイナリをビルド、`vX.Y.Z-beta.<run>` タグの GitHub プレリリースを作成、Docker イメージを GHCR にプッシュ |
| **Promote Release** | 手動 `workflow_dispatch` | `Cargo.toml` に対してバージョンを検証、リリースアーティファクトをビルド、安定版 GitHub リリースを作成、Docker `:latest` をプッシュ |

**バージョニング:** `Cargo.toml` の `version` フィールドに基づくセマンティックバージョニング。`master` への各マージでベータプレリリースが自動生成されます。安定版リリースを作るには、`Cargo.toml` をバンプしてマージし、一致するバージョンで _Promote Release_ をトリガーしてください。

**リリースターゲット:** `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-pc-windows-msvc`。

### ビルドのトラブルシューティング（Linux OpenSSL エラー）

`openssl-sys` のビルドエラーが発生した場合、依存関係を同期してリポジトリのロックファイルで再ビルドしてください:

```bash
git pull
cargo build --release --locked
cargo install --path . --force --locked
```

ZeroClaw は HTTP/TLS 依存関係に `rustls` を使用するよう設定されています。`--locked` は新しい環境でも推移的依存グラフを確定的に保ちます。

## コラボレーションとドキュメント

タスク指向のマップとして、ドキュメントハブからスタートしてください:

- ドキュメントハブ: [`docs/README.md`](docs/README.md)
- 統合ドキュメント TOC: [`docs/SUMMARY.md`](docs/SUMMARY.md)
- コマンドリファレンス: [`docs/reference/cli/commands-reference.md`](docs/reference/cli/commands-reference.md)
- 設定リファレンス: [`docs/reference/api/config-reference.md`](docs/reference/api/config-reference.md)
- Provider リファレンス: [`docs/reference/api/providers-reference.md`](docs/reference/api/providers-reference.md)
- Channel リファレンス: [`docs/reference/api/channels-reference.md`](docs/reference/api/channels-reference.md)
- 運用ガイド（Runbook）: [`docs/ops/operations-runbook.md`](docs/ops/operations-runbook.md)
- トラブルシューティング: [`docs/ops/troubleshooting.md`](docs/ops/troubleshooting.md)
- ドキュメント一覧/分類: [`docs/maintainers/docs-inventory.md`](docs/maintainers/docs-inventory.md)
- プロジェクト triage スナップショット（2026年2月18日時点）: [`docs/maintainers/project-triage-snapshot-2026-02-18.md`](docs/maintainers/project-triage-snapshot-2026-02-18.md)

コラボレーションの主要リファレンス:

- ドキュメントハブ: [docs/README.md](docs/README.md)
- ドキュメントテンプレート: [docs/contributing/doc-template.md](docs/contributing/doc-template.md)
- ドキュメント変更チェックリスト: [docs/README.md#4-documentation-change-checklist](docs/README.md#4-documentation-change-checklist)
- Channel 設定リファレンス: [docs/reference/api/channels-reference.md](docs/reference/api/channels-reference.md)
- Matrix 暗号化ルーム運用: [docs/security/matrix-e2ee-guide.md](docs/security/matrix-e2ee-guide.md)
- コントリビューションガイド: [CONTRIBUTING.md](CONTRIBUTING.md)
- PR ワークフローポリシー: [docs/contributing/pr-workflow.md](docs/contributing/pr-workflow.md)
- レビューアープレイブック（triage + ディープレビュー）: [docs/contributing/reviewer-playbook.md](docs/contributing/reviewer-playbook.md)
- セキュリティ開示ポリシー: [SECURITY.md](SECURITY.md)

デプロイおよびランタイム運用:

- ネットワークデプロイガイド: [docs/ops/network-deployment.md](docs/ops/network-deployment.md)
- プロキシエージェントプレイブック: [docs/ops/proxy-agent-playbook.md](docs/ops/proxy-agent-playbook.md)

## ZeroClaw を支援する

ZeroClaw があなたの仕事に役立ち、継続的な開発を支援したい場合は、こちらから寄付できます:

<a href="https://buymeacoffee.com/argenistherose"><img src="https://img.shields.io/badge/Buy%20Me%20a%20Coffee-Donate-yellow.svg?style=for-the-badge&logo=buy-me-a-coffee" alt="Buy Me a Coffee" /></a>

### 🙏 スペシャルサンクス

このオープンソースの取り組みにインスピレーションと力を与えてくれたコミュニティと機関に心から感謝します:

- **Harvard University** — 知的好奇心を育み、可能性の限界を押し広げてくれたことに感謝。
- **MIT** — オープンナレッジ、オープンソースを擁護し、テクノロジーは誰にでもアクセス可能であるべきという信念に感謝。
- **Sundai Club** — コミュニティ、エネルギー、そして重要なものを作り続ける不屈の意志に感謝。
- **世界とその先へ** 🌍✨ — オープンソースを良い力にしているすべてのコントリビューター、夢見る人、ビルダーへ。これはあなたのためのものです。

私たちは最高のアイデアはあらゆる場所から生まれると信じ、オープンに開発しています。これを読んでいるあなたも、その一部です。ようこそ。🦀❤️

## ⚠️ 公式リポジトリとなりすまし警告

**これが唯一の ZeroClaw 公式リポジトリです:**

> https://github.com/zeroclaw-labs/zeroclaw

他のリポジトリ、組織、ドメイン、パッケージで「ZeroClaw」を名乗ったり、ZeroClaw Labs との関連を示唆するものは、**無許可であり本プロジェクトとは一切関係ありません**。確認済みの無許可フォークは [TRADEMARK.md](docs/maintainers/trademark.md) に記載されます。

なりすましや商標の悪用を発見した場合は、[Issue を作成](https://github.com/zeroclaw-labs/zeroclaw/issues)してください。

---

## ライセンス

ZeroClaw は最大限のオープン性とコントリビューター保護のためにデュアルライセンスです:

| ライセンス | ユースケース |
|-----------|------------|
| [MIT](LICENSE-MIT) | オープンソース、研究、学術、個人利用 |
| [Apache 2.0](LICENSE-APACHE) | 特許保護、機関利用、商用デプロイ |

いずれかのライセンスを選択できます。**コントリビューターは両方のライセンス下で自動的に権利を付与します** — 完全なコントリビューター契約は [CLA.md](docs/contributing/cla.md) を参照してください。

### 商標

**ZeroClaw** の名称とロゴは ZeroClaw Labs の商標です。このライセンスは、推薦や提携を暗示するためにそれらを使用する許可を付与しません。許可・禁止される使用方法は [TRADEMARK.md](docs/maintainers/trademark.md) を参照してください。

### コントリビューター保護

- あなたのコントリビューションの**著作権は保持**されます
- **特許付与**（Apache 2.0）により他のコントリビューターからの特許請求から保護されます
- あなたのコントリビューションはコミット履歴と [NOTICE](NOTICE) に**永久に帰属**されます
- コントリビューションによって商標権は移転されません

## コントリビュート

ZeroClaw が初めてですか？ [`good first issue`](https://github.com/zeroclaw-labs/zeroclaw/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) ラベルの付いた Issue を探してください — 始め方は [Contributing Guide](CONTRIBUTING.md#first-time-contributors) を参照してください。

詳細は [CONTRIBUTING.md](CONTRIBUTING.md) と [CLA.md](docs/contributing/cla.md) を参照してください。Trait を実装して PR を提出:

- CI ワークフローガイド: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- 新しい `Provider` → `src/providers/`
- 新しい `Channel` → `src/channels/`
- 新しい `Observer` → `src/observability/`
- 新しい `Tool` → `src/tools/`
- 新しい `Memory` → `src/memory/`
- 新しい `Tunnel` → `src/tunnel/`
- 新しい `Skill` → `~/.zeroclaw/workspace/skills/<name>/`

---

**ZeroClaw** — Zero overhead. Zero compromise. Deploy anywhere. Swap anything. 🦀

## Contributors

<a href="https://github.com/zeroclaw-labs/zeroclaw/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=zeroclaw-labs/zeroclaw" alt="ZeroClaw contributors" />
</a>

## Star History

<p align="center">
  <a href="https://www.star-history.com/#zeroclaw-labs/zeroclaw&type=date&legend=top-left">
    <picture>
     <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=zeroclaw-labs/zeroclaw&type=date&theme=dark&legend=top-left" />
     <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=zeroclaw-labs/zeroclaw&type=date&legend=top-left" />
     <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=zeroclaw-labs/zeroclaw&type=date&legend=top-left" />
    </picture>
  </a>
</p>
