# Référence des providers (Français)

Cette page est une localisation initiale Wave 1 pour vérifier les IDs provider, alias et variables d'authentification.

Source anglaise:

- [../../providers-reference.md](../../providers-reference.md)

## Quand l'utiliser

- Choisir un provider et un modèle
- Vérifier ID/alias/env vars de credentials
- Diagnostiquer les erreurs de configuration/auth

## Règle

- Les IDs provider et noms d'env vars restent en anglais.
- La source normative de comportement est l'anglais.

## Notes de mise à jour

- Ajout d'un réglage `provider.reasoning_level` pour le niveau de raisonnement OpenAI Codex. Voir la source anglaise pour les détails.
- 2026-03-01: ajout de la prise en charge du provider StepFun (`stepfun`, alias `step`, `step-ai`, `step_ai`).

## StepFun (Résumé)

- Provider ID: `stepfun`
- Aliases: `step`, `step-ai`, `step_ai`
- Base API URL: `https://api.stepfun.com/v1`
- Endpoints: `POST /v1/chat/completions`, `GET /v1/models`
- Auth env var: `STEP_API_KEY` (fallback: `STEPFUN_API_KEY`)
- Modèle par défaut: `step-3.5-flash`

Validation rapide:

```bash
export STEP_API_KEY="your-stepfun-api-key"
zeroclaw models refresh --provider stepfun
zeroclaw agent --provider stepfun --model step-3.5-flash -m "ping"
```
