# ローカライズブリッジ: Pr Workflow

このページは強化版ブリッジです。テーマの位置付け、原文セクション導線、実行時の注意点をまとめています。

英語版原文:

- [../../pr-workflow.md](../../pr-workflow.md)

## テーマ位置付け

- 分類: エンジニアリング運用
- 深度: 強化ブリッジ（セクション導線 + 実行ヒント）
- 使い方: 構成を把握してから、英語版の規範記述に従って実施します。

## 原文セクションガイド

- [H2 · 0. Summary](../../pr-workflow.md#0-summary)
- [H2 · 1. Fast Path by PR Situation](../../pr-workflow.md#1-fast-path-by-pr-situation)
- [H3 · 1.1 Intake is incomplete](../../pr-workflow.md#1-1-intake-is-incomplete)
- [H3 · 1.2 `CI Required Gate` failing](../../pr-workflow.md#1-2-ci-required-gate-failing)
- [H3 · 1.3 High-risk path touched](../../pr-workflow.md#1-3-high-risk-path-touched)
- [H3 · 1.4 PR is superseded or duplicate](../../pr-workflow.md#1-4-pr-is-superseded-or-duplicate)
- [H2 · 2. Governance Goals and Control Loop](../../pr-workflow.md#2-governance-goals-and-control-loop)
- [H3 · 2.1 Governance goals](../../pr-workflow.md#2-1-governance-goals)
- [H3 · 2.2 Governance design logic (control loop)](../../pr-workflow.md#2-2-governance-design-logic-control-loop)
- [H2 · 3. Required Repository Settings](../../pr-workflow.md#3-required-repository-settings)
- [H2 · 4. PR Lifecycle Runbook](../../pr-workflow.md#4-pr-lifecycle-runbook)
- [H3 · 4.1 Step A: Intake](../../pr-workflow.md#4-1-step-a-intake)
- [H3 · 4.2 Step B: Validation](../../pr-workflow.md#4-2-step-b-validation)
- [H3 · 4.3 Step C: Review](../../pr-workflow.md#4-3-step-c-review)
- [H3 · 4.4 Step D: Merge](../../pr-workflow.md#4-4-step-d-merge)
- [H2 · 5. PR Readiness Contracts (DoR / DoD)](../../pr-workflow.md#5-pr-readiness-contracts-dor-dod)
- [H3 · 5.1 Definition of Ready (DoR) before requesting review](../../pr-workflow.md#5-1-definition-of-ready-dor-before-requesting-review)
- [H3 · 5.2 Definition of Done (DoD) merge-ready](../../pr-workflow.md#5-2-definition-of-done-dod-merge-ready)

## 実行ヒント

- まず原文の見出し構成を確認し、今回の変更範囲に直結する節から読みます。
- コマンド名、設定キー、API パス、コード識別子は英語のまま保持します。
- 仕様解釈に差分が出る場合は英語版原文を優先します。

## 関連エントリ

- [README.md](README.md)
- [SUMMARY.md](SUMMARY.md)
- [docs-inventory.md](docs-inventory.md)
