# ZeroClaw OpenShift deployment

Deploy a minimal ZeroClaw agent on OpenShift with an external LLM
provider (Anthropic, OpenAI, or any OpenAI-compatible API).

## Prerequisites

- `oc` CLI authenticated to your OpenShift cluster
- Container image pushed to an accessible registry
- OpenRouter API key, or edit the sample provider alias/env var names for your LLM provider

## Quick start

1. Copy the sample manifests to create your real ones:

   ```bash
   for f in deploy-k8s/*-sample.yaml; do cp "$f" "${f/-sample/}"; done
   ```

1. Edit `secret.yaml` and replace `REPLACE_WITH_YOUR_OPENROUTER_API_KEY` with
   your actual API key
1. Update the `image` field in `deployment.yaml` to point to your
   registry (e.g., `ghcr.io/youruser/zeroclaw:latest`)
1. Update the `namespace` in all files if you want a different name
1. Optionally edit `configmap.yaml` to change the provider or model
1. If you enable a channel with `always_ask` tool approval, set
   `approval_timeout_secs` explicitly in that channel block. The sample includes
   commented Telegram and Discord blocks with safe timeout values.
1. Apply all manifests:

   ```bash
   oc apply -f deploy-k8s/
   ```

The real `.yaml` files are gitignored so your secrets and
customizations stay local.

## Verification

Check that the pod is running and the route is accessible:

```bash
oc -n zeroclaw get pods
oc -n zeroclaw get route zeroclaw
```

Test the health endpoint:

```bash
ROUTE=$(oc -n zeroclaw get route zeroclaw -o jsonpath='{.spec.host}')
curl -sf "https://${ROUTE}/health"
```

Send a test message:

```bash
curl -X POST "https://${ROUTE}/webhook" \
  -H "Content-Type: application/json" \
  -d '{"message": "hello, what model are you?"}'
```

## Configuration

The shape of `configmap.yaml`'s embedded `config.toml` follows the canonical
[Provider Configuration → Minimal working example](../docs/book/src/providers/configuration.md#minimal-working-example).
The sample's aliases are `cloud` (provider entry) and `assistant` (agent +
risk profile) — substitute your own. Common edit points:

| Setting | Path in `config.toml` | Sample value |
| ------- | --------------------- | ------------ |
| Model | `providers.models.openrouter.cloud.model` | `anthropic/claude-sonnet-4-20250514` |
| Temperature | `providers.models.openrouter.cloud.temperature` | `0.7` |
| Autonomy level | `risk_profiles.assistant.level` | `supervised` |
| Runtime budget | `runtime_profiles.assistant.max_tool_iterations` | `8` |
| Agent → provider link | `agents.assistant.model_provider` | `openrouter.cloud` |

To swap to a different provider type (OpenAI, Anthropic, Ollama, etc.),
replace the `[providers.models.openrouter.cloud]` block with a
`[providers.models.<type>.<alias>]` entry from
[providers/catalog](../docs/book/src/providers/catalog.md) and update
`agents.assistant.model_provider` to match.

After editing, re-apply and restart the pod:

```bash
oc apply -f deploy-k8s/configmap.yaml
oc -n zeroclaw rollout restart deployment zeroclaw
```

## Notes

- **State is ephemeral.** Both `state` and `workspace` volumes use
  `emptyDir` — agent memory and session history do not persist across
  pod restarts. For production, replace these with
  PersistentVolumeClaims.
- **Vanilla Kubernetes.** The `Route` object is OpenShift-specific. On
  vanilla Kubernetes, replace `route-sample.yaml` with a Kubernetes
  Ingress targeting port 42617.

## Cleanup

```bash
oc delete namespace zeroclaw
```
