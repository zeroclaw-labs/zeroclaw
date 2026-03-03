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
- Ajout de `observability.runtime_trace_record_http` pour tracer les détails HTTP LLM (`llm_http_request` / `llm_http_response`) ; par défaut `false` ; effet uniquement lorsque `runtime_trace_mode` est `rolling` ou `full`. Payloads redactent des champs sensibles, mais fichiers de trace restent données opérationnelles sensibles. Requêtes/réponses/en-têtes tronqués si trop grands. Considérez désactiver en production. Référence canonique: `docs/config-reference.md`.

## `[observability]`

| Clé | Par défaut | But |
|---|---|---|
| `backend` | `none` | Backend d'observabilité : `none`, `noop`, `log`, `prometheus`, `otel`, `opentelemetry` ou `otlp` |
| `otel_endpoint` | `http://localhost:4318` | Endpoint OTLP HTTP utilisé quand backend est `otel` |
| `otel_service_name` | `zeroclaw` | Nom du service envoyé au collecteur OTLP |
| `runtime_trace_mode` | `none` | Mode de stockage runtime trace : `none`, `rolling` ou `full` |
| `runtime_trace_path` | `state/runtime-trace.jsonl` | Chemin JSONL runtime trace (relatif au workspace sauf si absolu) |
| `runtime_trace_max_entries` | `200` | Événements maximum conservés quand `runtime_trace_mode = "rolling"` |
| `runtime_trace_record_http` | `false` | Enregistrer les événements détaillés request/response HTTP LLM (`llm_http_request` / `llm_http_response`) dans runtime trace |

Notes :

- `backend = "otel"` utilise OTLP HTTP export avec un client exporter bloquant pour que les spans et metrics puissent être émises en sécurité depuis des contextes non-Tokio.
- Les alias `opentelemetry` et `otlp` pointent vers le même backend OTel.
- Runtime traces sont destinés au debug des échecs tool-call et payloads d'outils modèle malformés. Ils peuvent contenir du texte de sortie modèle, donc gardez-les désactivés par défaut sur les hôtes partagés.
- `runtime_trace_record_http` n'est efficace que lorsque `runtime_trace_mode` est `rolling` ou `full`.
  - HTTP trace payloads rédactent des champs sensibles courants (par exemple en-têtes Authorization et champs query/body de type token), mais traitent toujours les fichiers trace comme des données opérationnelles sensibles.
  - Les requêtes/réponses/valeurs d'en-tête sont tronquées si trop grandes. Cependant, le trafic LLM à fort volume avec de grandes réponses peut encore augmenter considérablement l'utilisation de la mémoire et la taille des fichiers trace.
  - Envisagez de désactiver le HTTP tracing dans les environnements de production.
- Query runtime traces avec :
  - `zeroclaw doctor traces --limit 20`
  - `zeroclaw doctor traces --event tool_call_result --contains \"error\"`
  - `zeroclaw doctor traces --event llm_http_response --contains \"500\"`
  - `zeroclaw doctor traces --id <trace-id>`

Exemple :

```toml
[observability]
backend = "otel"
otel_endpoint = "http://localhost:4318"
otel_service_name = "zeroclaw"
runtime_trace_mode = "rolling"
runtime_trace_path = "state/runtime-trace.jsonl"
runtime_trace_max_entries = 200
runtime_trace_record_http = true
```
