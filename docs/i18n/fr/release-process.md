# Passerelle de localisation: Release Process

Cette page est une passerelle enrichie. Elle fournit le positionnement du sujet, un guidage par sections source et des conseils d'exécution.

Source anglaise:

- [../../release-process.md](../../release-process.md)

## Positionnement du sujet

- Catégorie : Processus d'ingénierie
- Profondeur : passerelle enrichie (guidage de sections + conseils d'exécution)
- Usage : comprendre la structure puis appliquer les étapes selon la source normative anglaise.

## Plan des sections source

- [H2 · Release Goals](../../release-process.md#release-goals)
- [H2 · Standard Cadence](../../release-process.md#standard-cadence)
- [H2 · Workflow Contract](../../release-process.md#workflow-contract)
- [H2 · Maintainer Procedure](../../release-process.md#maintainer-procedure)
- [H3 · 1) Preflight on `main`](../../release-process.md#1-preflight-on-main)
- [H3 · 2) Run verification build (no publish)](../../release-process.md#2-run-verification-build-no-publish)
- [H3 · 3) Cut release tag](../../release-process.md#3-cut-release-tag)
- [H3 · 4) Monitor publish run](../../release-process.md#4-monitor-publish-run)
- [H3 · 5) Post-release validation](../../release-process.md#5-post-release-validation)
- [H3 · 6) Publish Homebrew Core formula (bot-owned)](../../release-process.md#6-publish-homebrew-core-formula-bot-owned)
- [H2 · Emergency / Recovery Path](../../release-process.md#emergency-recovery-path)
- [H2 · Operational Notes](../../release-process.md#operational-notes)

## Conseils d'exécution

- Commencer par la structure des sections source, puis cibler les parties directement liées au changement en cours.
- Les noms de commandes, clés de configuration, chemins API et identifiants de code restent en anglais.
- En cas d'ambiguïté d'interprétation, la source anglaise fait foi.

## Entrées liées

- [README.md](README.md)
- [SUMMARY.md](SUMMARY.md)
- [docs-inventory.md](docs-inventory.md)
