#!/usr/bin/env bash
# Prepare the release Docker build context used by CI and release workflows.
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
usage:
  prepare_docker_context.sh from-artifacts <context-dir> <artifact-dir>
  prepare_docker_context.sh smoke <context-dir>

Modes:
  from-artifacts  Extract release tarballs into the Docker context.
  smoke           Create minimal executable/web placeholders for no-push PR builds.
USAGE
}

mode="${1:-}"
context_dir="${2:-docker-ctx}"
artifact_dir="${3:-artifacts}"

if [[ "$mode" != "from-artifacts" && "$mode" != "smoke" ]]; then
  usage
  exit 64
fi

mkdir -p \
  "$context_dir/bin/amd64" \
  "$context_dir/bin/arm64" \
  "$context_dir/zeroclaw-data/.zeroclaw" \
  "$context_dir/zeroclaw-data/data"

case "$mode" in
  from-artifacts)
    tar xzf "$artifact_dir/zeroclaw-x86_64-unknown-linux-gnu.tar.gz" -C "$context_dir/bin/amd64"
    tar xzf "$artifact_dir/zeroclaw-aarch64-unknown-linux-gnu.tar.gz" -C "$context_dir/bin/arm64"
    for arch in amd64 arm64; do
      for bin in zeroclaw zerocode; do
        [[ -x "$context_dir/bin/$arch/$bin" ]] || {
          echo "missing executable: $context_dir/bin/$arch/$bin" >&2
          exit 1
        }
      done
      [[ -f "$context_dir/bin/$arch/web/dist/index.html" ]] || {
        echo "missing dashboard bundle: $context_dir/bin/$arch/web/dist/index.html" >&2
        exit 1
      }
    done
    ;;
  smoke)
    for arch in amd64 arm64; do
      mkdir -p "$context_dir/bin/$arch/web/dist"
      for bin in zeroclaw zerocode; do
        cat > "$context_dir/bin/$arch/$bin" <<EOF
#!/usr/bin/env sh
echo "$bin smoke binary"
EOF
        chmod +x "$context_dir/bin/$arch/$bin"
      done
      printf '<!doctype html><title>ZeroClaw smoke dashboard</title>\n' \
        > "$context_dir/bin/$arch/web/dist/index.html"
    done
    ;;
esac

printf '%s\n' \
  'api_key = ""' \
  'default_provider = "openrouter"' \
  'default_model = "anthropic/claude-sonnet-4-20250514"' \
  'default_temperature = 0.7' \
  '' \
  '[gateway]' \
  'port = 42617' \
  'host = "[::]"' \
  'allow_public_bind = true' \
  'web_dist_dir = "/usr/share/zeroclawlabs/web/dist"' \
  > "$context_dir/zeroclaw-data/.zeroclaw/config.toml"

rm -f "$context_dir/Dockerfile.debian"
cp Dockerfile.ci "$context_dir/Dockerfile"
