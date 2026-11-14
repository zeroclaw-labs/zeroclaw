#!/bin/sh
# Reproducible cold-build comparison: Dioxus WASM PoC vs React dashboard (#8132).
#
# Requirements: Linux, podman, git, GNU tar. Run from anywhere in the checkout:
#
#     sh web-wasm-poc/measure.sh
#
# What it does:
#   1. Builds one toolchain image from pinned digests, so both builds use the
#      same Rust compiler. Image setup is reported but not compared.
#   2. Copies the current checkout (tracked and untracked, non-ignored files)
#      into a fresh volume. The checkout itself is never modified.
#   3. Per build, downloads dependencies online into empty volumes, then
#      builds with networking disabled:
#        WASM:  web-wasm-poc/build.sh
#        React: cargo web build   (same command as the repo Dockerfile)
#   4. Appends one comment line to a source file and rebuilds:
#        WASM:  web-wasm-poc/build.sh again
#        React: npm run build in web/ (the API client is already generated).
#        Rerunning cargo web build after a web/src edit is not measured: the
#        gateway build.rs reruns `npm ci`, which needs the network.
#   5. Prints a Markdown report and writes logs to web-wasm-poc/measure-results/.
#
# Set KEEP_VOLUMES=1 to keep the build volumes for inspection.
set -eu

RUST_IMAGE=docker.io/library/rust:1.99.1-slim@sha256:11b01282eaac5ba4ab9fc503d3a4a4d5db601597ce85479ab8c29495be660670
NODE_IMAGE=docker.io/library/node:24.22.2-slim@sha256:a43f7fc3e52e87d12a905f46eea0275def84ca8f2981a5b53fed44d7fb398ee0

ROOT=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
cd "$ROOT"
WB=$(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/version = |"/, ""); print; exit }' web-wasm-poc/Cargo.lock)
IMAGE=localhost/zc-measure-toolchain:rust1.99.1-node24.22.2-wb$WB
OUT=$ROOT/web-wasm-poc/measure-results/$(date -u +%Y%m%dT%H%M%SZ)
P=zcm-$$
VOLS="$P-src $P-wasm-reg $P-wasm-git $P-web-reg $P-web-git"
mkdir -p "$OUT"
: > "$OUT/times.txt"

cleanup() {
    [ "${KEEP_VOLUMES:-0}" = 1 ] || podman volume rm -f $VOLS > /dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM HUP

step() {
    label=$1
    shift
    echo "==> $label"
    start=$(date +%s.%N)
    if ! "$@" > "$OUT/$label.log" 2>&1; then
        echo "FAILED: $label, log: $OUT/$label.log" >&2
        tail -20 "$OUT/$label.log" >&2
        exit 1
    fi
    end=$(date +%s.%N)
    awk -v a="$start" -v b="$end" -v l="$label" 'BEGIN { printf "%s %.1f\n", l, b - a }' >> "$OUT/times.txt"
}
seconds() { awk -v l="$1" '$1 == l { print $2 }' "$OUT/times.txt"; }

build_image() {
    context=$(mktemp -d)
    podman build -t "$IMAGE" -f - "$context" <<EOF
FROM $NODE_IMAGE AS node
FROM $RUST_IMAGE
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN apt-get update \\
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends g++ pkg-config \\
 && ln -s /usr/local/lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \\
 && ln -s /usr/local/lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx \\
 && rm -rf /var/lib/apt/lists/*
RUN rustup target add wasm32-unknown-unknown \\
 && cargo install --locked wasm-bindgen-cli --version $WB --bin wasm-bindgen \\
 && rm -rf /usr/local/cargo/registry /usr/local/cargo/git
EOF
    rm -rf "$context"
}

load_source() {
    for v in $VOLS; do podman volume create "$v" > /dev/null; done
    # The docs/book/po translation submodule is not needed by either build.
    git ls-files -z -co --exclude-standard \
        | grep -zv '^docs/book/po' \
        | tar --null --ignore-failed-read -T - -cf - \
        | podman run --rm -i --network=none -v "$P-src:/src" "$IMAGE" tar -xf - -C /src
}

# run online|offline SIDE SCRIPT; SIDE selects the wasm or web cargo volumes.
# online uses podman's default network.
run() {
    net=$1 side=$2 script=$3
    shift 3
    if [ "$net" = offline ]; then set -- --network=none "$@"; fi
    podman run --rm "$@" \
        -v "$P-src:/src" \
        -v "$P-$side-reg:/usr/local/cargo/registry" \
        -v "$P-$side-git:/usr/local/cargo/git" \
        -w /src "$IMAGE" sh -ec "$script"
}

measure_sizes() {
    run offline web '
sz() { printf "%s %s %s %s\n" "$1" "$2" "$(wc -c < "$3")" "$(gzip -9c "$3" | wc -c)"; }
W=web-wasm-poc
for f in index.html static/poc.css pkg/skills_poc.js pkg/skills_poc_bg.wasm \
         $(cd $W && find pkg/snippets -name "*.js" | sort); do
    sz wasm_page "$f" "$W/$f"
done
# React Skills page: entry assets named in index.html, the Skills chunk, and
# every chunk they import statically. Dynamic import() chunks are excluded.
D=web/dist
queue="$(grep -o "/_app/assets/[^\"]*\.\(js\|css\)" $D/index.html | sed "s#/_app/assets/##") $(ls $D/assets | grep "^Skills-.*\.js$")"
seen=" "
while [ -n "$(echo $queue)" ]; do
    set -- $queue; f=$1; shift; queue="$*"
    case "$seen" in *" $f "*) continue ;; esac
    seen="$seen$f "
    case $f in *.js) queue="$queue $(grep -o "\(from\|import\)\"\./[^\"]*\.js\"" $D/assets/$f | sed "s/.*\"\.\///; s/\"$//")" ;; esac
done
sz react_page index.html $D/index.html
for f in $seen; do sz react_page "assets/$f" "$D/assets/$f"; done
find $D -type f \( -name "*.js" -o -name "*.css" -o -name "*.html" \) | sort | while read -r f; do
    sz react_all "${f#$D/}" "$f"
done
'
}

dependencies() {
    run offline wasm '
cd web-wasm-poc
echo "wasm_lockfile $(grep -c "^name = " Cargo.lock)"
echo "wasm_compiled $(cargo tree --offline --locked --target wasm32-unknown-unknown -e normal,build --prefix none --no-dedupe | sed "s/ (\*)//" | sort -u | wc -l)"
cd ../web
node -e "const p = require(\"./package-lock.json\").packages; const k = Object.keys(p).filter(Boolean); console.log(\"npm_installed \" + k.length); console.log(\"npm_non_dev \" + k.filter(x => !p[x].dev).length)"
'
}

environment() {
    run offline web 'rustc -V; cargo -V; echo "node $(node -v)"; echo "npm $(npm -v)"; wasm-bindgen --version'
}

step setup_image build_image
step load_source load_source
step wasm_fetch run online wasm 'cd web-wasm-poc && cargo fetch --locked'
step wasm_cold run offline wasm 'cd web-wasm-poc && CARGO_NET_OFFLINE=true ./build.sh'
step wasm_edit run offline wasm 'cd web-wasm-poc && echo "// measure.sh edit" >> src/main.rs && CARGO_NET_OFFLINE=true ./build.sh'
step web_fetch run online web 'cd web && npm ci --ignore-scripts && cd .. && cargo fetch --locked'
step web_cold run offline web 'CARGO_NET_OFFLINE=true cargo web build'
step web_edit run offline web 'echo "// measure.sh edit" >> web/src/pages/Skills.tsx && cd web && npm run build'
step sizes measure_sizes
step dependencies dependencies
step environment environment

total() { awk -v g="$1" '$1 == g { r += $3; z += $4 } END { printf "%d / %d", r, z }' "$OUT/sizes.log"; }
vite() { grep -o 'built in [0-9.]*m\{0,1\}s' "$OUT/$1.log" | tail -1 | sed 's/built in //'; }
lines() { cat "$@" | wc -l | tr -d ' '; }
dep() { awk -v k="$1" '$1 == k { print $2 }' "$OUT/dependencies.log"; }
DIRTY=$(git status --porcelain | wc -l | tr -d ' ')
CPU=$(awk -F': ' '/^model name/ { print $2; exit }' /proc/cpuinfo)
MEM=$(awk '/^MemTotal/ { printf "%.0f GiB", $2 / 1048576 }' /proc/meminfo)

{
    echo "## Build and size measurements"
    echo
    echo "Revision \`$(git rev-parse --short=12 HEAD)\`, $DIRTY uncommitted path(s). $(date -u +%Y-%m-%dT%H:%MZ)."
    echo "Host: $CPU, $(nproc) threads, $MEM, $(uname -m), $(podman --version)."
    echo "Toolchain (same image for both builds): $(tr '\n' ',' < "$OUT/environment.log" | sed 's/,$//; s/,/, /g')."
    echo "Produced by \`sh web-wasm-poc/measure.sh\`. Every build starts from empty volumes."
    echo "Downloads run online. Builds run with \`--network=none\`."
    echo
    echo "### Build time (seconds, wall clock)"
    echo
    echo "| Step | WASM PoC | React |"
    echo "|---|---|---|"
    echo "| Download dependencies | $(seconds wasm_fetch) | $(seconds web_fetch) |"
    echo "| Cold build (\`build.sh\` / \`cargo web build\`) | $(seconds wasm_cold) | $(seconds web_cold) |"
    echo "| One-line edit, rebuild (\`build.sh\` / \`npm run build\`) | $(seconds wasm_edit) | $(seconds web_edit) |"
    echo
    echo "React cold build includes compiling \`xtask\` and the gateway to generate the API client."
    echo "Vite alone: $(vite web_cold) cold, $(vite web_edit) after the edit."
    echo
    echo "### Size (bytes, raw / gzip -9)"
    echo
    echo "| Payload | Bytes |"
    echo "|---|---|"
    echo "| WASM PoC page (html, css, js, wasm, snippets) | $(total wasm_page) |"
    echo "| React Skills page (index.html, entry assets, Skills chunk, static imports) | $(total react_page) |"
    echo "| React whole dashboard (all html, js, css) | $(total react_all) |"
    echo
    echo "Images and fonts are excluded from both. Per-file sizes: \`sizes.log\`."
    echo
    echo "### Dependencies"
    echo
    echo "| Count | Number |"
    echo "|---|---|"
    echo "| WASM PoC crates compiled for \`wasm32-unknown-unknown\` (normal and build deps) | $(dep wasm_compiled) |"
    echo "| WASM PoC \`Cargo.lock\` entries (all features and platforms, mostly never built) | $(dep wasm_lockfile) |"
    echo "| React npm packages installed by \`npm ci\` | $(dep npm_installed) |"
    echo "| React npm packages not marked dev | $(dep npm_non_dev) |"
    echo
    echo "### Source lines"
    echo
    echo "| Code | Lines |"
    echo "|---|---|"
    echo "| WASM PoC \`web-wasm-poc/src/*.rs\` | $(lines web-wasm-poc/src/*.rs) |"
    echo "| React \`web/src/pages/Skills.tsx\` + \`web/src/components/SkillCard.tsx\` | $(lines web/src/pages/Skills.tsx web/src/components/SkillCard.tsx) |"
    echo
    echo "Line counts include blank lines and comments. The PoC also contains token entry (\`auth.rs\`) and its own API client (\`api.rs\`)."
} | tee "$OUT/report.md"
echo
echo "Logs and report: $OUT"
