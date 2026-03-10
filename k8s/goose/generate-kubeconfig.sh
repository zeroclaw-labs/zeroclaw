#!/usr/bin/env bash
# Generate a time-bound kubeconfig for the goose todolist configurator.
#
# Usage:
#   ./generate-kubeconfig.sh              # 24h token (default)
#   ./generate-kubeconfig.sh 48h          # custom duration
#   ./generate-kubeconfig.sh 24h renew    # renew: update token in existing kubeconfig
#
# The generated kubeconfig grants:
#   - Full control of the todolist namespace
#   - Read-only access to cluster-scoped resources (nodes, storageclasses, CRDs)
#   - No access to other namespaces' workloads or secrets

set -euo pipefail

DURATION="${1:-24h}"
MODE="${2:-}"
KUBECONFIG_FILE="goose-todolist.kubeconfig"
SA_NAME="goose-todolist-configurator"
SA_NAMESPACE="ai-agents"
CLUSTER_NAME="scrapyard"

# Generate time-bound token
TOKEN=$(kubectl create token "${SA_NAME}" -n "${SA_NAMESPACE}" --duration="${DURATION}")

if [[ "${MODE}" == "renew" && -f "${KUBECONFIG_FILE}" ]]; then
  # Renew: just update the token in the existing kubeconfig
  kubectl config set-credentials goose-todolist \
    --token="${TOKEN}" \
    --kubeconfig="${KUBECONFIG_FILE}"
  echo "Token renewed for ${DURATION}. Kubeconfig: ${KUBECONFIG_FILE}"
  exit 0
fi

# Get cluster info from current context
API_SERVER=$(kubectl config view --minify -o jsonpath='{.clusters[0].cluster.server}')
CA_DATA=$(kubectl config view --minify --raw -o jsonpath='{.clusters[0].cluster.certificate-authority-data}')

# Build the kubeconfig
kubectl config set-cluster "${CLUSTER_NAME}" \
  --server="${API_SERVER}" \
  --embed-certs=true \
  --kubeconfig="${KUBECONFIG_FILE}" \
  --certificate-authority=<(echo "${CA_DATA}" | base64 -d)

kubectl config set-credentials goose-todolist \
  --token="${TOKEN}" \
  --kubeconfig="${KUBECONFIG_FILE}"

kubectl config set-context goose-todolist \
  --cluster="${CLUSTER_NAME}" \
  --user=goose-todolist \
  --namespace=todolist \
  --kubeconfig="${KUBECONFIG_FILE}"

kubectl config use-context goose-todolist \
  --kubeconfig="${KUBECONFIG_FILE}"

echo "Kubeconfig generated: ${KUBECONFIG_FILE}"
echo "Token expires in: ${DURATION}"
echo "Default namespace: todolist"
echo ""
echo "To renew later:  ./generate-kubeconfig.sh ${DURATION} renew"
echo "To use:          export KUBECONFIG=\$(pwd)/${KUBECONFIG_FILE}"
