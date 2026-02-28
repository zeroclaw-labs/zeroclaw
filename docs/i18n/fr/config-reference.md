# Référence de configuration (Français)

Cette page est une localisation initiale Wave 1 pour les clés de configuration et les valeurs par défaut.

Source anglaise:

- [../../config-reference.md](../../config-reference.md)

## Quand l'utiliser

- Initialiser un nouvel environnement
- Vérifier les conflits de configuration
- Auditer les paramètres de sécurité/stabilité

## Règle

- Les noms de clés de configuration restent en anglais.
- Le comportement runtime exact est défini en anglais.

## Notes de mise à jour

- Ajout de `provider.reasoning_level` (OpenAI Codex `/responses`). Voir la source anglaise pour les détails.
- Valeur par défaut de `agent.max_tool_iterations` augmentée à `20` (fallback sûr si `0`).
