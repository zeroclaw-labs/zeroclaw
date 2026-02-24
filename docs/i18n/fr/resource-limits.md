# Passerelle de localisation: Resource Limits

Cette page est une passerelle enrichie. Elle fournit le positionnement du sujet, un guidage par sections source et des conseils d'exécution.

Source anglaise:

- [../../resource-limits.md](../../resource-limits.md)

## Positionnement du sujet

- Catégorie : Sécurité et gouvernance
- Profondeur : passerelle enrichie (guidage de sections + conseils d'exécution)
- Usage : comprendre la structure puis appliquer les étapes selon la source normative anglaise.

## Plan des sections source

- [H2 · Problem](../../resource-limits.md#problem)
- [H2 · Proposed Solutions](../../resource-limits.md#proposed-solutions)
- [H3 · Option 1: cgroups v2 (Linux, Recommended)](../../resource-limits.md#option-1-cgroups-v2-linux-recommended)
- [H3 · Option 2: tokio::task::deadlock detection](../../resource-limits.md#option-2-tokio-task-deadlock-detection)
- [H3 · Option 3: Memory monitoring](../../resource-limits.md#option-3-memory-monitoring)
- [H2 · Config Schema](../../resource-limits.md#config-schema)
- [H2 · Implementation Priority](../../resource-limits.md#implementation-priority)

## Conseils d'exécution

- Commencer par la structure des sections source, puis cibler les parties directement liées au changement en cours.
- Les noms de commandes, clés de configuration, chemins API et identifiants de code restent en anglais.
- En cas d'ambiguïté d'interprétation, la source anglaise fait foi.

## Entrées liées

- [README.md](README.md)
- [SUMMARY.md](SUMMARY.md)
- [docs-inventory.md](docs-inventory.md)
