#!/usr/bin/env bash
# deploy-sg-dev.sh — Build, push, and deploy ZeroClaw to the Aliyun sg-dev cluster.
#
# Replicates what `.github/workflows/build-one2x.yml` does, but runs
# locally. Use this when GitHub Actions can't run (rate limits, account
# restrictions, offline development) OR when you want a faster edit→verify
# loop than waiting for CI.
#
# # What it does
#
#   1. Compute an image tag from git HEAD (`v6.3.0-<short-sha>`).
#   2. `docker buildx` a linux/amd64 image from `Dockerfile.ci`.
#   3. Log in to Aliyun ACR (uses existing docker-config creds; falls back
#      to ALICLOUD_ACCESS_KEY / ALICLOUD_SECRET_KEY if present).
#   4. Push the image to `loveops-prod-acr-registry.ap-southeast-1.cr.aliyuncs.com`.
#   5. Update `videoclaw-ops/apps/zeroclaw/dev/manifests.yaml` in-place
#      (both the `ZEROCLAW_IMAGE` env value and any `image:` line).
#   6. Git commit + push videoclaw-ops (skip with `--no-gitops`).
#   7. `kubectl apply` the manifest to sg-dev to skip ArgoCD wait time.
#   8. Restart `agent-orchestrator` so it picks up the new ZEROCLAW_IMAGE env.
#   9. Watch the rollout until it's Running.
#
# # Usage
#
#   ./scripts/deploy-sg-dev.sh                 # build, push, deploy, verify
#   ./scripts/deploy-sg-dev.sh --skip-build    # just push cached image + deploy
#   ./scripts/deploy-sg-dev.sh --skip-push     # build only (dry-run-ish)
#   ./scripts/deploy-sg-dev.sh --tag v6.3.1-hotfix
#   ./scripts/deploy-sg-dev.sh --no-gitops     # don't commit to videoclaw-ops
#   ./scripts/deploy-sg-dev.sh --no-apply      # don't kubectl apply (GitOps-only)
#
# # Requirements
#
#   - docker with buildx (for macOS→linux/amd64 cross-build)
#   - kubectl with `sg-dev` context configured
#   - videoclaw-ops repo cloned alongside zeroclaw (auto-detected)
#   - ACR login (one of):
#       a) already logged in (`docker login` cached in ~/.docker/config.json)
#       b) env vars ALICLOUD_ACCESS_KEY + ALICLOUD_SECRET_KEY
#
# Exit codes: 0 = success, non-zero = failed step (loud error).

set -euo pipefail

# ── Constants (match .github/workflows/build-one2x.yml) ───────────
ACR_REGISTRY="loveops-prod-acr-registry.ap-southeast-1.cr.aliyuncs.com"
ACR_INSTANCE_ID="cri-e71dfjucxw8ipc7m"
ACR_REGION="ap-southeast-1"
IMAGE_NAME="platform/zeroclaw"
DOCKERFILE="Dockerfile.ci"
PLATFORM="linux/amd64"

# Deployment targets
K8S_CONTEXT="sg-dev"
K8S_NAMESPACE="zeroclaw-dev"
MANIFEST_PATH_REL="apps/zeroclaw/dev/manifests.yaml"

# Flags
SKIP_BUILD=0
SKIP_PUSH=0
NO_GITOPS=0
NO_APPLY=0
TAG=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag)           TAG="$2"; shift 2 ;;
    --skip-build)    SKIP_BUILD=1; shift ;;
    --skip-push)     SKIP_PUSH=1; shift ;;
    --no-gitops)     NO_GITOPS=1; shift ;;
    --no-apply)      NO_APPLY=1; shift ;;
    -h|--help)
      sed -n '2,30p' "$0"
      exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

# ── Resolve paths ─────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ZC_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# videoclaw-ops is expected alongside zeroclaw
OPS_ROOT="${VIDEOCLAW_OPS:-$ZC_ROOT/../videoclaw-ops}"
if [[ ! -d "$OPS_ROOT" ]]; then
  echo "❌ videoclaw-ops repo not found at $OPS_ROOT" >&2
  echo "   Set VIDEOCLAW_OPS env var to override, or clone the repo alongside zeroclaw." >&2
  exit 1
fi
MANIFEST_PATH="$OPS_ROOT/$MANIFEST_PATH_REL"
if [[ ! -f "$MANIFEST_PATH" ]]; then
  echo "❌ Manifest not found: $MANIFEST_PATH" >&2
  exit 1
fi

# ── Compute tag from git HEAD (match CI convention) ───────────────
cd "$ZC_ROOT"
if [[ -z "$TAG" ]]; then
  SHORT_SHA=$(git rev-parse --short=7 HEAD)
  TAG="v6.3.0-${SHORT_SHA}"
fi
FULL_IMAGE="${ACR_REGISTRY}/${IMAGE_NAME}:${TAG}"

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  ZeroClaw → sg-dev deploy"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Tag:        $TAG"
echo "  Image:      $FULL_IMAGE"
echo "  ZC repo:    $ZC_ROOT"
echo "  ops repo:   $OPS_ROOT"
echo "  manifest:   $MANIFEST_PATH_REL"
echo "  k8s:        $K8S_CONTEXT/$K8S_NAMESPACE"
echo "  flags:      skip-build=$SKIP_BUILD skip-push=$SKIP_PUSH no-gitops=$NO_GITOPS no-apply=$NO_APPLY"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo

# ── Sanity: working tree clean for reproducibility ────────────────
if [[ -n "$(git -C "$ZC_ROOT" status --porcelain)" ]]; then
  echo "⚠️  zeroclaw working tree has uncommitted changes."
  echo "    The image will reflect HEAD ($SHORT_SHA), NOT the dirty tree."
  echo "    Press Enter to continue, Ctrl+C to abort."
  read -r
fi

# ── Step 1: docker build ──────────────────────────────────────────
if [[ $SKIP_BUILD -eq 0 ]]; then
  echo "▶ [1/5] docker buildx build ($PLATFORM)"
  # Ensure a buildx builder exists (idempotent)
  docker buildx inspect default-amd64 >/dev/null 2>&1 || \
    docker buildx create --name default-amd64 --driver docker-container >/dev/null
  docker buildx use default-amd64

  docker buildx build \
    --platform "$PLATFORM" \
    -f "$DOCKERFILE" \
    -t "$FULL_IMAGE" \
    --load \
    "$ZC_ROOT"
  echo "   ✓ built $FULL_IMAGE"
else
  echo "▶ [1/5] docker build (SKIPPED via --skip-build)"
fi
echo

# ── Step 2: ACR login ─────────────────────────────────────────────
if [[ $SKIP_PUSH -eq 0 ]]; then
  echo "▶ [2/5] ACR login"
  # Detection order:
  #   1. `~/.docker/config.json` has an `auths` entry for the registry.
  #      This is where Docker Desktop (credsStore=desktop / osxkeychain)
  #      lists registries whose creds live in the keychain — `docker
  #      system info` does NOT surface them.
  #   2. ALICLOUD_ACCESS_KEY / ALICLOUD_SECRET_KEY env vars (CI path).
  DOCKER_CFG="${DOCKER_CONFIG:-$HOME/.docker}/config.json"
  if [[ -f "$DOCKER_CFG" ]] && grep -q "\"${ACR_REGISTRY}\"" "$DOCKER_CFG"; then
    echo "   ✓ already logged in to $ACR_REGISTRY (cached)"
  elif docker system info 2>/dev/null | grep -q "Server Address:.*${ACR_REGISTRY}"; then
    echo "   ✓ already logged in to $ACR_REGISTRY (docker-info)"
  elif [[ -n "${ALICLOUD_ACCESS_KEY:-}" && -n "${ALICLOUD_SECRET_KEY:-}" ]]; then
    echo "   using ALICLOUD_ACCESS_KEY / ALICLOUD_SECRET_KEY from env"
    echo "$ALICLOUD_SECRET_KEY" | docker login "$ACR_REGISTRY" \
      -u "$ALICLOUD_ACCESS_KEY" --password-stdin
  else
    echo "   ⚠ Not logged in and no env creds. Run one of:"
    echo "     docker login $ACR_REGISTRY"
    echo "     export ALICLOUD_ACCESS_KEY=... ALICLOUD_SECRET_KEY=... && $0"
    exit 1
  fi
else
  echo "▶ [2/5] ACR login (SKIPPED via --skip-push)"
fi
echo

# ── Step 3: docker push ───────────────────────────────────────────
if [[ $SKIP_PUSH -eq 0 ]]; then
  echo "▶ [3/5] docker push"
  docker push "$FULL_IMAGE"
  echo "   ✓ pushed $FULL_IMAGE"
else
  echo "▶ [3/5] docker push (SKIPPED via --skip-push)"
fi
echo

# ── Step 4: update videoclaw-ops manifest ─────────────────────────
echo "▶ [4/5] update $MANIFEST_PATH_REL"
cd "$OPS_ROOT"

# The CI uses sed. Do the same, matching both the env `value:` line and
# any top-level `image:` line for zeroclaw.
if sed --version >/dev/null 2>&1; then
  # GNU sed
  SED_INPLACE=(-i)
else
  # BSD sed (macOS)
  SED_INPLACE=(-i '')
fi
sed "${SED_INPLACE[@]}" \
  -e "s|value: \".*platform/zeroclaw:.*\"|value: \"${FULL_IMAGE}\"|g" \
  -e "s|image: .*platform/zeroclaw:.*|image: ${FULL_IMAGE}|g" \
  "$MANIFEST_PATH_REL"

if git diff --quiet "$MANIFEST_PATH_REL"; then
  echo "   ⚠ manifest already at $TAG — no change"
else
  echo "   ✓ manifest patched"
  git --no-pager diff --stat "$MANIFEST_PATH_REL"

  if [[ $NO_GITOPS -eq 0 ]]; then
    git add "$MANIFEST_PATH_REL"
    git commit -m "deploy: upgrade ZeroClaw to ${TAG} (local)

Deployed via scripts/deploy-sg-dev.sh from zeroclaw commit $(git -C "$ZC_ROOT" rev-parse HEAD).
"
    if git push 2>&1 | tail -2; then
      echo "   ✓ videoclaw-ops push OK"
    else
      echo "   ⚠ videoclaw-ops push failed — continuing with direct apply"
    fi
  else
    echo "   (--no-gitops) manifest edited but NOT committed"
  fi
fi
echo

# ── Step 5: trigger ArgoCD sync + wait ────────────────────────────
#
# This cluster is managed by ArgoCD. Running `kubectl apply` directly
# creates a race: our `apply` mutates the live spec, ArgoCD sees drift
# against the last-known-good git SHA, and reverts. The correct GitOps
# path is:
#
#   1. Push manifest change to git (step 4 already did this).
#   2. Ask ArgoCD to sync *to HEAD*, overriding any latency it has.
#   3. Wait for rollout.
#
# Fallback when ArgoCD isn't reachable (e.g. the `argocd` namespace
# doesn't exist on this context): fall back to direct `kubectl apply`.
ARGO_APP_NAME="${ARGO_APP_NAME:-zeroclaw-dev}"
if [[ $NO_APPLY -eq 0 ]]; then
  echo "▶ [5/5] ArgoCD sync + rollout"

  if kubectl --context "$K8S_CONTEXT" -n argocd \
       get application "$ARGO_APP_NAME" >/dev/null 2>&1; then
    echo "   triggering ArgoCD sync on application/$ARGO_APP_NAME to HEAD"
    kubectl --context "$K8S_CONTEXT" -n argocd patch application "$ARGO_APP_NAME" \
      --type=merge \
      -p '{"operation":{"initiatedBy":{"username":"deploy-sg-dev.sh"},"sync":{"revision":"HEAD","prune":true,"syncStrategy":{"hook":{}}}}}' \
      >/dev/null
    # Small pause to let ArgoCD pick up the operation request.
    sleep 3
    # Poll until sync status is Synced with our target revision OR timeout.
    SYNC_DEADLINE=$(( $(date +%s) + 120 ))
    while true; do
      SYNC_STATUS=$(kubectl --context "$K8S_CONTEXT" -n argocd \
        get application "$ARGO_APP_NAME" \
        -o jsonpath='{.status.sync.status}' 2>/dev/null || echo "")
      SYNC_REV=$(kubectl --context "$K8S_CONTEXT" -n argocd \
        get application "$ARGO_APP_NAME" \
        -o jsonpath='{.status.sync.revision}' 2>/dev/null || echo "")
      if [[ "$SYNC_STATUS" == "Synced" ]]; then
        echo "   ✓ ArgoCD Synced at ${SYNC_REV:0:12}"
        break
      fi
      if (( $(date +%s) > SYNC_DEADLINE )); then
        echo "   ⚠ ArgoCD didn't reach Synced within 120s (current: $SYNC_STATUS); continuing anyway"
        break
      fi
      sleep 3
    done
  else
    echo "   ArgoCD not found on this cluster — falling back to kubectl apply"
    kubectl --context "$K8S_CONTEXT" apply -f "$MANIFEST_PATH"
  fi

  echo "   waiting for deployment rollout..."
  if kubectl --context "$K8S_CONTEXT" -n "$K8S_NAMESPACE" \
       rollout status deployment/agent-orchestrator --timeout=300s; then
    echo "   ✓ agent-orchestrator rollout complete"
  else
    echo "   ❌ rollout timed out — check \`kubectl describe deployment agent-orchestrator -n $K8S_NAMESPACE\`"
    exit 1
  fi

  # Verify ZEROCLAW_IMAGE env var actually reflects our new tag. If ArgoCD
  # raced us (rare), this will be visibly wrong instead of silently OK.
  LIVE_IMAGE=$(kubectl --context "$K8S_CONTEXT" -n "$K8S_NAMESPACE" \
    get deployment agent-orchestrator \
    -o jsonpath='{.spec.template.spec.containers[0].env[?(@.name=="ZEROCLAW_IMAGE")].value}' 2>/dev/null || echo "")
  if [[ "$LIVE_IMAGE" == "$FULL_IMAGE" ]]; then
    echo "   ✓ cluster ZEROCLAW_IMAGE matches target: $TAG"
  else
    echo "   ⚠ cluster ZEROCLAW_IMAGE drift: expected $FULL_IMAGE, got $LIVE_IMAGE"
    echo "     (Re-run this script, or check ArgoCD in the UI.)"
  fi

  echo
  echo "Current pods in $K8S_NAMESPACE:"
  kubectl --context "$K8S_CONTEXT" -n "$K8S_NAMESPACE" get pods
else
  echo "▶ [5/5] ArgoCD sync (SKIPPED via --no-apply)"
fi
echo

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  ✅ ZeroClaw $TAG deployed to $K8S_CONTEXT"
echo "  Image: $FULL_IMAGE"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
