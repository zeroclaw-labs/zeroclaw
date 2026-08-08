#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
guard="$script_dir/web_rsc_mode_guard.sh"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT

reset_fixture() {
  rm -rf "$fixture/web" "$fixture/.github"
  mkdir -p "$fixture/web/src" "$fixture/.github/workflows"
  cat >"$fixture/.github/workflows/npm-deps-review.yml" <<'YAML'
jobs:
  review:
    steps:
      - uses: actions/dependency-review-action@example
        with:
          allow-ghsas: GHSA-qwww-vcr4-c8h2
YAML
  cat >"$fixture/web/package.json" <<'JSON'
{
  "dependencies": {
    "react-router-dom": "7.18.2"
  },
  "devDependencies": {
    "vite": "7.0.0"
  }
}
JSON
  cat >"$fixture/web/src/main.tsx" <<'TSX'
import { BrowserRouter } from "react-router-dom";

export { BrowserRouter };
TSX
  cat >"$fixture/web/index.html" <<'HTML'
<div id="root"></div>
<script type="module" src="/src/main.tsx"></script>
HTML
  cat >"$fixture/web/vite.config.ts" <<'TS'
import { defineConfig } from "vite";
import path from "path";

export default defineConfig({
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
});
TS
}

run_guard() {
  ZEROCLAW_RSC_GUARD_ROOT="$fixture" \
    ZEROCLAW_RSC_GUARD_TODAY="2026-08-02" \
    ZEROCLAW_RSC_GUARD_EXPIRES_OVERRIDE="2026-09-01" \
    bash "$guard"
}

expect_failure() {
  local description="$1"
  if run_guard >/dev/null 2>&1; then
    echo "web-rsc-mode-guard test failed: $description was accepted" >&2
    exit 1
  fi
}

reset_fixture
run_guard >/dev/null

reset_fixture
node - "$fixture/.github/workflows/npm-deps-review.yml" <<'NODE'
const fs = require("node:fs");
const workflowPath = process.argv[2];
const workflow = fs.readFileSync(workflowPath, "utf8");
fs.writeFileSync(
  workflowPath,
  workflow.replace("GHSA-qwww-vcr4-c8h2", "GHSA-xxxx-yyyy-zzzz"),
);
NODE
expect_failure "different advisory exception"

reset_fixture
node - "$fixture/.github/workflows/npm-deps-review.yml" <<'NODE'
const fs = require("node:fs");
const workflowPath = process.argv[2];
const workflow = fs.readFileSync(workflowPath, "utf8");
fs.writeFileSync(
  workflowPath,
  workflow.replace(
    "GHSA-qwww-vcr4-c8h2",
    "GHSA-qwww-vcr4-c8h2, GHSA-xxxx-yyyy-zzzz",
  ),
);
NODE
expect_failure "additional advisory exception"

reset_fixture
cat >"$fixture/web/src/node-test.ts" <<'TS'
import test from "node:test";

export { test };
TS
run_guard >/dev/null

reset_fixture
for section in dependencies devDependencies optionalDependencies peerDependencies; do
  node - "$fixture/web/package.json" "$section" <<'NODE'
const fs = require("node:fs");
const packagePath = process.argv[2];
const section = process.argv[3];
const pkg = JSON.parse(fs.readFileSync(packagePath, "utf8"));
pkg[section] = { ...(pkg[section] ?? {}), "@vitejs/plugin-rsc": "latest" };
fs.writeFileSync(packagePath, `${JSON.stringify(pkg, null, 2)}\n`);
NODE
  expect_failure "RSC dependency in $section"
  reset_fixture
done

reset_fixture
node - "$fixture/web/package.json" <<'NODE'
const fs = require("node:fs");
const packagePath = process.argv[2];
const pkg = JSON.parse(fs.readFileSync(packagePath, "utf8"));
pkg.dependencies["react-router"] = "8.3.0";
fs.writeFileSync(packagePath, `${JSON.stringify(pkg, null, 2)}\n`);
NODE
expect_failure "direct react-router dependency"

reset_fixture
cat >"$fixture/web/src/server.ts" <<'TS'
import { reactRouter } from "@react-router/dev/vite";

export { reactRouter };
TS
expect_failure "server-capable React Router import"

reset_fixture
cat >"$fixture/web/src/server.ts" <<'TS'
import { StaticRouter } from "react-router-dom/server";

export { StaticRouter };
TS
expect_failure "React Router DOM server subpath import"

reset_fixture
cat >"$fixture/web/src/server.ts" <<'TS'
import { unstable_createCallServer } from "react-router-dom";

export { unstable_createCallServer };
TS
expect_failure "RSC API re-exported by react-router-dom"

reset_fixture
cat >"$fixture/web/src/server.jsx" <<'JSX'
import rsc from "@vitejs/plugin-rsc";

export default rsc;
JSX
expect_failure "RSC import in JSX source"

reset_fixture
cat >"$fixture/web/entry.rsc.mjs" <<'JS'
export default {};
JS
expect_failure "RSC entry filename"

reset_fixture
cat >"$fixture/web/src/main.tsx" <<'TSX'
// RSCStaticRouter is mentioned here only to verify that the conservative guard fails closed.
import { BrowserRouter } from "react-router-dom";

export { BrowserRouter };
TSX
expect_failure "RSC API marker in executable source"

reset_fixture
mkdir -p "$fixture/outside"
cat >"$fixture/outside/bridge.ts" <<'TS'
import { unstable_createCallServer } from "react-router-dom";

export { unstable_createCallServer };
TS
ln -s "$fixture/outside/bridge.ts" "$fixture/web/src/bridge.ts"
expect_failure "symbolic link escaping the web root"

reset_fixture
cat >"$fixture/web/vite.config.ts" <<'TS'
import { defineConfig } from "vite";
import path from "path";

export default defineConfig({
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "../outside"),
    },
  },
});
TS
expect_failure "Vite alias escaping the web root"

reset_fixture
cat >"$fixture/web/src/main.tsx" <<'TSX'
import { page } from "local-page";

export { page };
TSX
expect_failure "undeclared local alias"

reset_fixture
mkdir -p "$fixture/outside"
cat >"$fixture/outside/bridge.ts" <<'TS'
export const bridge = true;
TS
cat >"$fixture/web/src/main.tsx" <<'TSX'
import { bridge } from "../../outside/bridge";

export { bridge };
TSX
expect_failure "relative import escaping the web root"

reset_fixture
mkdir -p "$fixture/outside"
cat >"$fixture/outside/bridge.ts" <<'TS'
export const bridge = true;
TS
cat >"$fixture/web/src/main.tsx" <<'TSX'
import { bridge } from "@/../../outside/bridge";

export { bridge };
TSX
expect_failure "alias import escaping the web root"

reset_fixture
cat >"$fixture/web/src/main.tsx" <<'TSX'
const router = await import(`react-router-dom`);

export { router };
TSX
expect_failure "template-literal dynamic react-router-dom namespace"

reset_fixture
cat >"$fixture/web/src/main.tsx" <<'TSX'
"use server";

export const action = () => true;
TSX
expect_failure "server directive"

reset_fixture
if ZEROCLAW_RSC_GUARD_ROOT="$fixture" \
  ZEROCLAW_RSC_GUARD_TODAY="2026-09-01" \
  ZEROCLAW_RSC_GUARD_EXPIRES_OVERRIDE="2026-09-01" \
  bash "$guard" >/dev/null 2>&1; then
  echo "web-rsc-mode-guard test failed: expired exception was accepted" >&2
  exit 1
fi

reset_fixture
if ZEROCLAW_RSC_GUARD_ROOT="$fixture" \
  ZEROCLAW_RSC_GUARD_TODAY="2026-02-31" \
  ZEROCLAW_RSC_GUARD_EXPIRES_OVERRIDE="2026-09-01" \
  bash "$guard" >/dev/null 2>&1; then
  echo "web-rsc-mode-guard test failed: invalid calendar date was accepted" >&2
  exit 1
fi

if ZEROCLAW_RSC_GUARD_TODAY="2026-08-02" bash "$guard" >/dev/null 2>&1; then
  echo "web-rsc-mode-guard test failed: non-fixture current-date override was accepted" >&2
  exit 1
fi

echo "web-rsc-mode-guard tests passed"
