# Passerelle de localisation: Sandboxing

Cette page est une passerelle enrichie. Elle fournit le positionnement du sujet, un guidage par sections source et des conseils d'exécution.

Source anglaise:

- [../../sandboxing.md](../../sandboxing.md)

## Positionnement du sujet

- Catégorie : Sécurité et gouvernance
- Profondeur : passerelle enrichie (guidage de sections + conseils d'exécution)
- Usage : comprendre la structure puis appliquer les étapes selon la source normative anglaise.

## Plan des sections source

- [H2 · Problem](../../sandboxing.md#problem)
- [H2 · Proposed Solutions](../../sandboxing.md#proposed-solutions)
- [H3 · Option 1: Firejail Integration (Recommended for Linux)](../../sandboxing.md#option-1-firejail-integration-recommended-for-linux)
- [H3 · Option 2: Bubblewrap (Portable, no root required)](../../sandboxing.md#option-2-bubblewrap-portable-no-root-required)
- [H3 · Option 3: Docker-in-Docker (Heavyweight but complete isolation)](../../sandboxing.md#option-3-docker-in-docker-heavyweight-but-complete-isolation)
- [H3 · Option 4: Landlock (Linux Kernel LSM, Rust native)](../../sandboxing.md#option-4-landlock-linux-kernel-lsm-rust-native)
- [H2 · Priority Implementation Order](../../sandboxing.md#priority-implementation-order)
- [H2 · Config Schema Extension](../../sandboxing.md#config-schema-extension)
- [H2 · Testing Strategy](../../sandboxing.md#testing-strategy)

## Conseils d'exécution

- Commencer par la structure des sections source, puis cibler les parties directement liées au changement en cours.
- Les noms de commandes, clés de configuration, chemins API et identifiants de code restent en anglais.
- En cas d'ambiguïté d'interprétation, la source anglaise fait foi.

## Entrées liées

- [README.md](README.md)
- [SUMMARY.md](SUMMARY.md)
- [docs-inventory.md](docs-inventory.md)
