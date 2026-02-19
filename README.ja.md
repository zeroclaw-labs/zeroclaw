<p align="center">
  <img src="zeroclaw.png" alt="ZeroClaw" width="200" />
</p>

<h1 align="center">ZeroClaw 🦀（日本語）</h1>

<p align="center">
  <strong>Zero overhead. Zero compromise. 100% Rust. 100% Agnostic.</strong>
</p>

<p align="center">
  <a href="https://x.com/zeroclawlabs?s=21"><img src="https://img.shields.io/badge/X-%40zeroclawlabs-000000?style=flat&logo=x&logoColor=white" alt="X: @zeroclawlabs" /></a>
  <a href="https://www.xiaohongshu.com/user/profile/67cbfc43000000000d008307?xsec_token=AB73VnYnGNx5y36EtnnZfGmAmS-6Wzv8WMuGpfwfkg6Yc%3D&xsec_source=pc_search"><img src="https://img.shields.io/badge/Xiaohongshu-Official-FF2442?style=flat" alt="Xiaohongshu: Official" /></a>
  <a href="https://t.me/zeroclawlabs"><img src="https://img.shields.io/badge/Telegram-%40zeroclawlabs-26A5E4?style=flat&logo=telegram&logoColor=white" alt="Telegram: @zeroclawlabs" /></a>
  <a href="https://www.reddit.com/r/zeroclawlabs/"><img src="https://img.shields.io/badge/Reddit-r%2Fzeroclawlabs-FF4500?style=flat&logo=reddit&logoColor=white" alt="Reddit: r/zeroclawlabs" /></a>
</p>

<p align="center">
  🌐 言語: <a href="README.md">English</a> · <a href="README.zh-CN.md">简体中文</a> · <a href="README.ja.md">日本語</a> · <a href="README.ru.md">Русский</a>
</p>

<p align="center">
  <a href="bootstrap.sh">ワンクリック導入</a> |
  <a href="docs/getting-started/README.md">導入ガイド</a> |
  <a href="docs/README.ja.md">ドキュメントハブ</a> |
  <a href="docs/SUMMARY.md">Docs TOC</a>
</p>

<p align="center">
  <strong>クイック分流：</strong>
  <a href="docs/reference/README.md">参照</a> ·
  <a href="docs/operations/README.md">運用</a> ·
  <a href="docs/troubleshooting.md">障害対応</a> ·
  <a href="docs/security/README.md">セキュリティ</a> ·
  <a href="docs/hardware/README.md">ハードウェア</a> ·
  <a href="docs/contributing/README.md">貢献・CI</a>
</p>

> この文書は `README.md` の内容を、正確性と可読性を重視して日本語に整えた版です（逐語訳ではありません）。
>
> コマンド名、設定キー、API パス、Trait 名などの技術識別子は英語のまま維持しています。
>
> 最終同期日: **2026-02-19**。

## 📢 お知らせボード

重要なお知らせ（互換性破壊変更、セキュリティ告知、メンテナンス時間、リリース阻害事項など）をここに掲載します。

| 日付 (UTC) | レベル | お知らせ | 対応 |
|---|---|---|---|
| 2026-02-19 | _緊急_ | 私たちは `openagen/zeroclaw` および `zeroclaw.org` とは**一切関係ありません**。`zeroclaw.org` は現在 `openagen/zeroclaw` の fork を指しており、そのドメイン/リポジトリは当プロジェクトの公式サイト・公式プロジェクトを装っています。 | これらの情報源による案内、バイナリ、資金調達情報、公式発表は信頼しないでください。必ず本リポジトリと認証済み公式SNSのみを参照してください。 |
| 2026-02-19 | _重要_ | 公式サイトは**まだ公開しておらず**、なりすましの試みを確認しています。ZeroClaw 名義の投資・資金調達などの活動には参加しないでください。 | 情報は本リポジトリを最優先で確認し、[X（@zeroclawlabs）](https://x.com/zeroclawlabs?s=21)、[Reddit（r/zeroclawlabs）](https://www.reddit.com/r/zeroclawlabs/)、[Telegram（@zeroclawlabs）](https://t.me/zeroclawlabs) と [小紅書アカウント](https://www.xiaohongshu.com/user/profile/67cbfc43000000000d008307?xsec_token=AB73VnYnGNx5y36EtnnZfGmAmS-6Wzv8WMuGpfwfkg6Yc%3D&xsec_source=pc_search) で公式更新を確認してください。 |
| 2026-02-19 | _重要_ | Anthropic は 2026-02-19 に Authentication and Credential Use を更新しました。条文では、OAuth authentication（Free/Pro/Max）は Claude Code と Claude.ai 専用であり、Claude Free/Pro/Max で取得した OAuth トークンを他の製品・ツール・サービス（Agent SDK を含む）で使用することは許可されず、Consumer Terms of Service 違反に該当すると明記されています。 | 損失回避のため、当面は Claude Code OAuth 連携を試さないでください。原文: [Authentication and Credential Use](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use)。 |

## 概要

ZeroClaw は、高速・省リソース・高拡張性を重視した自律エージェント実行基盤です。

- Rust ネイティブ実装、単一バイナリで配布可能
- Trait ベース設計（`Provider` / `Channel` / `Tool` / `Memory` など）
- セキュアデフォルト（ペアリング、明示 allowlist、サンドボックス、スコープ制御）

## ZeroClaw が選ばれる理由

- **軽量ランタイムを標準化**: CLI や `status` などの常用操作は数MB級メモリで動作。
- **低コスト環境に適合**: 低価格ボードや小規模クラウドでも、重い実行基盤なしで運用可能。
- **高速コールドスタート**: Rust 単一バイナリにより、主要コマンドと daemon 起動が非常に速い。
- **高い移植性**: ARM / x86 / RISC-V を同じ運用モデルで扱え、provider/channel/tool を差し替え可能。

## ベンチマークスナップショット（ZeroClaw vs OpenClaw、再現可能）

以下はローカルのクイック比較（macOS arm64、2026年2月）を、0.8GHz エッジ CPU 基準で正規化したものです。

| | OpenClaw | NanoBot | PicoClaw | ZeroClaw 🦀 |
|---|---|---|---|---|
| **言語** | TypeScript | Python | Go | **Rust** |
| **RAM** | > 1GB | > 100MB | < 10MB | **< 5MB** |
| **起動時間（0.8GHz コア）** | > 500s | > 30s | < 1s | **< 10ms** |
| **バイナリサイズ** | ~28MB（dist） | N/A（スクリプト） | ~8MB | **3.4 MB** |
| **コスト** | Mac Mini $599 | Linux SBC ~$50 | Linux ボード $10 | **任意の $10 ハードウェア** |

> 注記: ZeroClaw の結果は release ビルドを `/usr/bin/time -l` で計測したものです。OpenClaw は Node.js ランタイムが必要で、ランタイム由来だけで通常は約390MBの追加メモリを要します。NanoBot は Python ランタイムが必要です。PicoClaw と ZeroClaw は静的バイナリです。

<p align="center">
  <img src="zero-claw.jpeg" alt="ZeroClaw vs OpenClaw Comparison" width="800" />
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

- Release バイナリ: `8.8M`
- `zeroclaw --help`: 約 `0.02s`、ピークメモリ 約 `3.9MB`
- `zeroclaw status`: 約 `0.01s`、ピークメモリ 約 `4.1MB`

## ワンクリック導入

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./bootstrap.sh
```

環境ごと初期化する場合: `./bootstrap.sh --install-system-deps --install-rust`（システムパッケージで `sudo` が必要な場合があります）。

詳細は [`docs/one-click-bootstrap.md`](docs/one-click-bootstrap.md) を参照してください。

## クイックスタート

### Homebrew（macOS/Linuxbrew）

```bash
brew install zeroclaw
```

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
cargo build --release --locked
cargo install --path . --force --locked

zeroclaw onboard --api-key sk-... --provider openrouter
zeroclaw onboard --interactive

zeroclaw agent -m "Hello, ZeroClaw!"

# default: 127.0.0.1:3000
zeroclaw gateway

zeroclaw daemon
```

## セキュリティのデフォルト

- Gateway の既定バインド: `127.0.0.1:3000`
- 既定でペアリング必須: `require_pairing = true`
- 既定で公開バインド禁止: `allow_public_bind = false`
- Channel allowlist:
  - `[]` は deny-by-default
  - `["*"]` は allow all（意図的に使う場合のみ）

## 設定例

```toml
api_key = "sk-..."
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4-6"
default_temperature = 0.7

[memory]
backend = "sqlite"
auto_save = true
embedding_provider = "none"

[gateway]
host = "127.0.0.1"
port = 3000
require_pairing = true
allow_public_bind = false
```

## ドキュメント入口

- ドキュメントハブ（英語）: [`docs/README.md`](docs/README.md)
- 統合 TOC: [`docs/SUMMARY.md`](docs/SUMMARY.md)
- ドキュメントハブ（日本語）: [`docs/README.ja.md`](docs/README.ja.md)
- コマンドリファレンス: [`docs/commands-reference.md`](docs/commands-reference.md)
- 設定リファレンス: [`docs/config-reference.md`](docs/config-reference.md)
- Provider リファレンス: [`docs/providers-reference.md`](docs/providers-reference.md)
- Channel リファレンス: [`docs/channels-reference.md`](docs/channels-reference.md)
- 運用ガイド（Runbook）: [`docs/operations-runbook.md`](docs/operations-runbook.md)
- トラブルシューティング: [`docs/troubleshooting.md`](docs/troubleshooting.md)
- ドキュメント一覧 / 分類: [`docs/docs-inventory.md`](docs/docs-inventory.md)
- プロジェクト triage スナップショット: [`docs/project-triage-snapshot-2026-02-18.md`](docs/project-triage-snapshot-2026-02-18.md)

## コントリビュート / ライセンス

- Contributing: [`CONTRIBUTING.md`](CONTRIBUTING.md)
- PR Workflow: [`docs/pr-workflow.md`](docs/pr-workflow.md)
- Reviewer Playbook: [`docs/reviewer-playbook.md`](docs/reviewer-playbook.md)
- License: MIT（[`LICENSE`](LICENSE), [`NOTICE`](NOTICE)）

---

詳細仕様（全コマンド、アーキテクチャ、API 仕様、開発フロー）は英語版の [`README.md`](README.md) を参照してください。
