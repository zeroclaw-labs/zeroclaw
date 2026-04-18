# ZeroClaw OpenShift deployment

Deploy a minimal ZeroClaw agent on OpenShift with an external LLM
provider (Anthropic, OpenAI, or any OpenAI-compatible API).

## Prerequisites

- `oc` CLI authenticated to your OpenShift cluster
- Container image pushed to an accessible registry
- API key for your LLM provider

## Quick start

1. Copy the sample manifests to create your real ones:

   ```bash
   for f in deploy-k8s/*-sample.yaml; do cp "$f" "${f/-sample/}"; done
   ```

1. Edit `secret.yaml` and replace `REPLACE_WITH_YOUR_API_KEY` with
   your actual API key
1. Update the `image` field in `deployment.yaml` to point to your
   registry (e.g., `ghcr.io/youruser/zeroclaw:latest`)
1. Update the `namespace` in all files if you want a different name
1. Optionally edit `configmap.yaml` to change the provider or model
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

Edit `configmap.yaml` to change runtime settings:

| Setting | Field | Default |
| ------- | ----- | ------- |
| LLM provider | `default_provider` | `anthropic` |
| Model | `default_model` | `claude-sonnet-4-20250514` |
| Temperature | `default_temperature` | `0.7` |
| Autonomy level | `autonomy.level` | `supervised` |

After editing, re-apply and restart the pod:

```bash
oc apply -f deploy-k8s/configmap.yaml
oc -n zeroclaw rollout restart deployment zeroclaw
```

## Cleanup

```bash
oc delete namespace zeroclaw
```
