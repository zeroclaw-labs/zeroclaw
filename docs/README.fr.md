# Hub de Documentation ZeroClaw

Cette page est le point d'entrée principal du système de documentation.

Dernière mise à jour : **20 février 2026**.

Hubs localisés : [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Tiếng Việt](i18n/vi/README.md).

## Commencez Ici

| Je veux…                                                            | Lire ceci                                                                      |
| ------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| Installer et exécuter ZeroClaw rapidement                           | [README.md (Démarrage Rapide)](../README.md#quick-start)                       |
| Bootstrap en une seule commande                                     | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md)                   |
| Trouver des commandes par tâche                                     | [commands-reference.md](reference/cli/commands-reference.md)                   |
| Vérifier rapidement les valeurs par défaut et clés de config        | [config-reference.md](reference/api/config-reference.md)                       |
| Configurer des fournisseurs/endpoints personnalisés                 | [custom-providers.md](contributing/custom-providers.md)                         |
| Configurer le fournisseur Z.AI / GLM                                | [zai-glm-setup.md](setup-guides/zai-glm-setup.md)                              |
| Utiliser les modèles d'intégration LangGraph                        | [langgraph-integration.md](contributing/langgraph-integration.md)               |
| Opérer le runtime (runbook jour-2)                                  | [operations-runbook.md](ops/operations-runbook.md)                             |
| Dépanner les problèmes d'installation/runtime/canal                 | [troubleshooting.md](ops/troubleshooting.md)                                   |
| Exécuter la configuration et diagnostics de salles chiffrées Matrix | [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md)                           |
| Parcourir les docs par catégorie                                    | [SUMMARY.md](SUMMARY.md)                                                       |
| Voir l'instantané docs des PR/issues du projet                      | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## Arbre de Décision Rapide (10 secondes)

- Besoin de configuration ou installation initiale ? → [setup-guides/README.md](setup-guides/README.md)
- Besoin de clés CLI/config exactes ? → [reference/README.md](reference/README.md)
- Besoin d'opérations de production/service ? → [ops/README.md](ops/README.md)
- Vous voyez des échecs ou régressions ? → [troubleshooting.md](ops/troubleshooting.md)
- Vous travaillez sur le durcissement sécurité ou la roadmap ? → [security/README.md](security/README.md)
- Vous travaillez avec des cartes/périphériques ? → [hardware/README.md](hardware/README.md)
- Contribution/revue/workflow CI ? → [contributing/README.md](contributing/README.md)
- Vous voulez la carte complète ? → [SUMMARY.md](SUMMARY.md)

## Collections (Recommandées)

- Démarrage : [setup-guides/README.md](setup-guides/README.md)
- Catalogues de référence : [reference/README.md](reference/README.md)
- Opérations & déploiement : [ops/README.md](ops/README.md)
- Docs sécurité : [security/README.md](security/README.md)
- Matériel/périphériques : [hardware/README.md](hardware/README.md)
- Contribution/CI : [contributing/README.md](contributing/README.md)
- Instantanés projet : [maintainers/README.md](maintainers/README.md)

## Par Audience

### Utilisateurs / Opérateurs

- [commands-reference.md](reference/cli/commands-reference.md) — recherche de commandes par workflow
- [providers-reference.md](reference/api/providers-reference.md) — IDs fournisseurs, alias, variables d'environnement d'identifiants
- [channels-reference.md](reference/api/channels-reference.md) — capacités des canaux et chemins de configuration
- [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md) — configuration de salles chiffrées Matrix (E2EE) et diagnostics de non-réponse
- [config-reference.md](reference/api/config-reference.md) — clés de configuration à haute signalisation et valeurs par défaut sécurisées
- [custom-providers.md](contributing/custom-providers.md) — modèles d'intégration de fournisseur personnalisé/URL de base
- [zai-glm-setup.md](setup-guides/zai-glm-setup.md) — configuration Z.AI/GLM et matrice d'endpoints
- [langgraph-integration.md](contributing/langgraph-integration.md) — intégration de secours pour les cas limites de modèle/appel d'outil
- [operations-runbook.md](ops/operations-runbook.md) — opérations runtime jour-2 et flux de rollback
- [troubleshooting.md](ops/troubleshooting.md) — signatures d'échec courantes et étapes de récupération

### Contributeurs / Mainteneurs

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### Sécurité / Fiabilité

> Note : cette zone inclut des docs de proposition/roadmap. Pour le comportement actuel, commencez par [config-reference.md](reference/api/config-reference.md), [operations-runbook.md](ops/operations-runbook.md), et [troubleshooting.md](ops/troubleshooting.md).

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [audit-logging.md](security/audit-logging.md)
- [resource-limits.md](ops/resource-limits.md)
- [security-roadmap.md](security/security-roadmap.md)

## Navigation Système & Gouvernance

- Table des matières unifiée : [SUMMARY.md](SUMMARY.md)
- Carte de structure docs (langue/partie/fonction) : [structure/README.md](maintainers/structure-README.md)
- Inventaire/classification de la documentation : [docs-inventory.md](maintainers/docs-inventory.md)
- Instantané de triage du projet : [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md)

## Autres langues

- English: [README.md](README.md)
- 简体中文: [README.zh-CN.md](README.zh-CN.md)
- 日本語: [README.ja.md](README.ja.md)
- Русский: [README.ru.md](README.ru.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
