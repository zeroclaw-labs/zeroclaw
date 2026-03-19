<p align="center">
  <img src="docs/assets/jhedaiclaw.png" alt="JhedaiClaw" width="200" />
</p>

<h1 align="center">JhedaiClaw 🦀（日本語）</h1>

<p align="center">
  <strong>Zero overhead. Zero compromise. 100% Rust. 100% Agnostic.</strong>
</p>

<p align="center">
  <a href="LICENSE-APACHE"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache%202.0-blue.svg" alt="License: MIT OR Apache-2.0" /></a>
  <a href="NOTICE"><img src="https://img.shields.io/badge/contributors-27+-green.svg" alt="Contributors" /></a>
  <a href="https://buymeacoffee.com/argenistherose"><img src="https://img.shields.io/badge/Buy%20Me%20a%20Coffee-Donate-yellow.svg?style=flat&logo=buy-me-a-coffee" alt="Buy Me a Coffee" /></a>
  <a href="https://x.com/jhedaiclaw?s=21"><img src="https://img.shields.io/badge/X-%40jhedaiclaw-000000?style=flat&logo=x&logoColor=white" alt="X: @jhedaiclaw" /></a>
  <a href="https://www.facebook.com/groups/jhedaiclaw"><img src="https://img.shields.io/badge/Facebook-Group-1877F2?style=flat&logo=facebook&logoColor=white" alt="Facebook Group" /></a>
  <a href="https://www.reddit.com/r/jhedaiclaw/"><img src="https://img.shields.io/badge/Reddit-r%2Fjhedaiclaw-FF4500?style=flat&logo=reddit&logoColor=white" alt="Reddit: r/jhedaiclaw" /></a>
</p>

<p align="center">
  🌐 言語:
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
  <a href="https://raw.githubusercontent.com/jhedai/jhedaiclaw/master/install.sh">ワンクリック導入</a> |
  <a href="docs/setup-guides/README.md">導入ガイド</a> |
  <a href="docs/README.ja.md">ドキュメントハブ</a> |
  <a href="docs/SUMMARY.md">Docs TOC</a>
</p>

<p align="center">
  <strong>クイック分流：</strong>
  <a href="docs/reference/README.md">参照</a> ·
  <a href="docs/ops/README.md">運用</a> ·
  <a href="docs/ops/troubleshooting.md">障害対応</a> ·
  <a href="docs/security/README.md">セキュリティ</a> ·
  <a href="docs/hardware/README.md">ハードウェア</a> ·
  <a href="docs/contributing/README.md">貢献・CI</a>
</p>

> この文書は `README.md` の内容を、正確性と可読性を重視して日本語に整えた版です（逐語訳ではありません）。
>
> コマンド名、設定キー、API パス、Trait 名などの技術識別子は英語のまま維持しています。
>
> 最終同期日: **2026-02-19**。

<!-- BEGIN:WHATS_NEW -->

### 🚀 What's New in v0.3.1 (March 2026)

| Area | Highlights                                        |
| ---- | ------------------------------------------------- |
| ci   | add Termux (aarch64-linux-android) release target |

<!-- END:WHATS_NEW -->

## 📢 お知らせボード

重要なお知らせ（互換性破壊変更、セキュリティ告知、メンテナンス時間、リリース阻害事項など）をここに掲載します。

| 日付 (UTC) | レベル | お知らせ                                                                                                                                                                                                                                                                                                                                                 | 対応                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| ---------- | ------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-02-19 | _緊急_ | 私たちは `openagen/jhedaiclaw` および `jhedaiclaw.org` とは**一切関係ありません**。`jhedaiclaw.org` は現在 `openagen/jhedaiclaw` の fork を指しており、そのドメイン/リポジトリは当プロジェクトの公式サイト・公式プロジェクトを装っています。                                                                                                             | これらの情報源による案内、バイナリ、資金調達情報、公式発表は信頼しないでください。必ず[本リポジトリ](https://github.com/jhedai/jhedaiclaw)と認証済み公式SNSのみを参照してください。                                                                                                                                                                                                                                                                                                                                             |
| 2026-02-21 | _重要_ | 公式サイトを公開しました: [jhedaiclaw.ai](https://jhedaiclaw.ai)。公開までお待ちいただきありがとうございました。引き続きなりすましの試みを確認しているため、JhedaiClaw 名義の投資・資金調達などの案内は、公式チャネルで確認できない限り参加しないでください。                                                                                            | 情報は[本リポジトリ](https://github.com/jhedai/jhedaiclaw)を最優先で確認し、[X（@jhedaiclaw）](https://x.com/jhedaiclaw?s=21)、[Telegram（@jhedaiclaw）](https://t.me/jhedaiclaw)、[Facebook（グループ）](https://www.facebook.com/groups/jhedaiclaw)、[Reddit（r/jhedaiclaw）](https://www.reddit.com/r/jhedaiclaw/) と [小紅書アカウント](https://www.xiaohongshu.com/user/profile/67cbfc43000000000d008307?xsec_token=AB73VnYnGNx5y36EtnnZfGmAmS-6Wzv8WMuGpfwfkg6Yc%3D&xsec_source=pc_search) で公式更新を確認してください。 |
| 2026-02-19 | _重要_ | Anthropic は 2026-02-19 に Authentication and Credential Use を更新しました。条文では、OAuth authentication（Free/Pro/Max）は Claude Code と Claude.ai 専用であり、Claude Free/Pro/Max で取得した OAuth トークンを他の製品・ツール・サービス（Agent SDK を含む）で使用することは許可されず、Consumer Terms of Service 違反に該当すると明記されています。 | 損失回避のため、当面は Claude Code OAuth 連携を試さないでください。原文: [Authentication and Credential Use](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use)。                                                                                                                                                                                                                                                                                                                          |

## 概要

JhedaiClaw は、高速・省リソース・高拡張性を重視した自律エージェント実行基盤です。JhedaiClawはエージェントワークフローのための**ランタイムオペレーティングシステム**です — モデル、ツール、メモリ、実行を抽象化し、エージェントを一度構築すればどこでも実行できるインフラストラクチャです。

- Rust ネイティブ実装、単一バイナリで配布可能
- Trait ベース設計（`Provider` / `Channel` / `Tool` / `Memory` など）
- セキュアデフォルト（ペアリング、明示 allowlist、サンドボックス、スコープ制御）

## JhedaiClaw が選ばれる理由

- **軽量ランタイムを標準化**: CLI や `status` などの常用操作は数MB級メモリで動作。
- **低コスト環境に適合**: 低価格ボードや小規模クラウドでも、重い実行基盤なしで運用可能。
- **高速コールドスタート**: Rust 単一バイナリにより、主要コマンドと daemon 起動が非常に速い。
- **高い移植性**: ARM / x86 / RISC-V を同じ運用モデルで扱え、provider/channel/tool を差し替え可能。

## ベンチマークスナップショット（JhedaiClaw vs OpenClaw、再現可能）

以下はローカルのクイック比較（macOS arm64、2026年2月）を、0.8GHz エッジ CPU 基準で正規化したものです。

|                             | OpenClaw      | NanoBot           | PicoClaw         | JhedaiClaw 🦀               |
| --------------------------- | ------------- | ----------------- | ---------------- | --------------------------- |
| **言語**                    | TypeScript    | Python            | Go               | **Rust**                    |
| **RAM**                     | > 1GB         | > 100MB           | < 10MB           | **< 5MB**                   |
| **起動時間（0.8GHz コア）** | > 500s        | > 30s             | < 1s             | **< 10ms**                  |
| **バイナリサイズ**          | ~28MB（dist） | N/A（スクリプト） | ~8MB             | **~8.8 MB**                 |
| **コスト**                  | Mac Mini $599 | Linux SBC ~$50    | Linux ボード $10 | **任意の $10 ハードウェア** |

> 注記: JhedaiClaw の結果は release ビルドを `/usr/bin/time -l` で計測したものです。OpenClaw は Node.js ランタイムが必要で、ランタイム由来だけで通常は約390MBの追加メモリを要します。NanoBot は Python ランタイムが必要です。PicoClaw と JhedaiClaw は静的バイナリです。

<p align="center">
  <img src="docs/assets/jhedaiclaw-comparison.jpeg" alt="JhedaiClaw vs OpenClaw Comparison" width="800" />
</p>

### ローカルで再現可能な測定

ベンチマーク値はコードやツールチェーン更新で変わるため、必ず自身の環境で再測定してください。

```bash
cargo build --release
ls -lh target/release/jhedaiclaw

/usr/bin/time -l target/release/jhedaiclaw --help
/usr/bin/time -l target/release/jhedaiclaw status
```

README のサンプル値（macOS arm64, 2026-02-18）:

- Release バイナリ: `8.8M`
- `jhedaiclaw --help`: 約 `0.02s`、ピークメモリ 約 `3.9MB`
- `jhedaiclaw status`: 約 `0.01s`、ピークメモリ 約 `4.1MB`

## ワンクリック導入

```bash
git clone https://github.com/jhedai/jhedaiclaw.git
cd jhedaiclaw
./install.sh
```

環境ごと初期化する場合: `./install.sh --install-system-deps --install-rust`（システムパッケージで `sudo` が必要な場合があります）。

詳細は [`docs/setup-guides/one-click-bootstrap.md`](docs/setup-guides/one-click-bootstrap.md) を参照してください。

## クイックスタート

### Homebrew（macOS/Linuxbrew）

```bash
brew install jhedaiclaw
```

```bash
git clone https://github.com/jhedai/jhedaiclaw.git
cd jhedaiclaw
cargo build --release --locked
cargo install --path . --force --locked

jhedaiclaw onboard --api-key sk-... --provider openrouter
jhedaiclaw onboard

jhedaiclaw agent -m "Hello, JhedaiClaw!"

# default: 127.0.0.1:42617
jhedaiclaw gateway

jhedaiclaw daemon
```

## Subscription Auth（OpenAI Codex / Claude Code）

JhedaiClaw はサブスクリプションベースのネイティブ認証プロファイルをサポートしています（マルチアカウント対応、保存時暗号化）。

- 保存先: `~/.jhedaiclaw/auth-profiles.json`
- 暗号化キー: `~/.jhedaiclaw/.secret_key`
- Profile ID 形式: `<provider>:<profile_name>`（例: `openai-codex:work`）

OpenAI Codex OAuth（ChatGPT サブスクリプション）:

```bash
# サーバー/ヘッドレス環境向け推奨
jhedaiclaw auth login --provider openai-codex --device-code

# ブラウザ/コールバックフロー（ペーストフォールバック付き）
jhedaiclaw auth login --provider openai-codex --profile default
jhedaiclaw auth paste-redirect --provider openai-codex --profile default

# 確認 / リフレッシュ / プロファイル切替
jhedaiclaw auth status
jhedaiclaw auth refresh --provider openai-codex --profile default
jhedaiclaw auth use --provider openai-codex --profile work
```

Claude Code / Anthropic setup-token:

```bash
# サブスクリプション/setup token の貼り付け（Authorization header モード）
jhedaiclaw auth paste-token --provider anthropic --profile default --auth-kind authorization

# エイリアスコマンド
jhedaiclaw auth setup-token --provider anthropic --profile default
```

Subscription auth で agent を実行:

```bash
jhedaiclaw agent --provider openai-codex -m "hello"
jhedaiclaw agent --provider openai-codex --auth-profile openai-codex:work -m "hello"

# Anthropic は API key と auth token の両方の環境変数をサポート:
# ANTHROPIC_AUTH_TOKEN, ANTHROPIC_OAUTH_TOKEN, ANTHROPIC_API_KEY
jhedaiclaw agent --provider anthropic -m "hello"
```

## コントリビュート / ライセンス

- Contributing: [`CONTRIBUTING.md`](CONTRIBUTING.md)
- PR Workflow: [`docs/contributing/pr-workflow.md`](docs/contributing/pr-workflow.md)
- Reviewer Playbook: [`docs/contributing/reviewer-playbook.md`](docs/contributing/reviewer-playbook.md)
- License: MIT or Apache 2.0（[`LICENSE-MIT`](LICENSE-MIT), [`LICENSE-APACHE`](LICENSE-APACHE), [`NOTICE`](NOTICE)）

<!-- BEGIN:RECENT_CONTRIBUTORS -->

### 🌟 Recent Contributors (v0.3.1)

3 contributors shipped features, fixes, and improvements in this release cycle:

- **Argenis**
- **argenis de la rosa**
- **Claude Opus 4.6**

Thank you to everyone who opened issues, reviewed PRs, translated docs, and helped test. Every contribution matters. 🦀

<!-- END:RECENT_CONTRIBUTORS -->

---

詳細仕様（全コマンド、アーキテクチャ、API 仕様、開発フロー）は英語版の [`README.md`](README.md) を参照してください。
