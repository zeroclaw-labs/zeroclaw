#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
guard="$script_dir/web_rsc_mode_guard.sh"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT

reset_fixture() {
  rm -rf "$fixture/web" "$fixture/.github"
  mkdir -p "$fixture/web" "$fixture/.github/workflows"
  cat >"$fixture/.github/workflows/ci.yml" <<'YAML'
jobs:
  npm-dependency-review:
    if: github.event_name == 'pull_request'
    steps:
      - uses: actions/dependency-review-action@3b139cfc5fae8b618d3eae3675e383bb1769c019
        with:
          fail-on-severity: high
          allow-ghsas: GHSA-qwww-vcr4-c8h2
  web-permission-tests:
    needs: [path-changes]
    if: needs.path-changes.outputs.web == 'true'
    steps:
      - name: Run RSC semantic guard
        working-directory: web
        run: npm run test:rsc-guard
  gate:
    if: always()
    needs: [npm-dependency-review, web-permission-tests]
    steps:
      - name: Check results
        run: |
          if [[ "${{ github.event_name }}" == "pull_request" && "${{ needs.npm-dependency-review.result }}" != "success" ]]; then
            echo "::error::npm dependency review did not complete successfully"
            exit 1
          fi
YAML
  cat >"$fixture/web/package.json" <<'JSON'
{
  "dependencies": {
    "react-router-dom": "7.18.2"
  },
  "devDependencies": {
    "vite": "8.0.16"
  }
}
JSON
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

for key in warn-only vulnerability-check config-file; do
  reset_fixture
  node - "$fixture/.github/workflows/ci.yml" "$key" <<'NODE'
const fs = require("node:fs");
const workflowPath = process.argv[2];
const key = process.argv[3];
const values = {
  "warn-only": "true",
  "vulnerability-check": "true",
  "config-file": ".github/dependency-review.yml",
};
const workflow = fs.readFileSync(workflowPath, "utf8");
const marker = "          allow-ghsas: GHSA-qwww-vcr4-c8h2";
fs.writeFileSync(workflowPath, workflow.replace(marker, `${marker}\n          ${key}: ${values[key]}`));
NODE
  expect_failure "additional dependency-review with key: $key"
done

reset_fixture
node - "$fixture/.github/workflows/ci.yml" <<'NODE'
const fs = require("node:fs");
const workflowPath = process.argv[2];
const workflow = fs.readFileSync(workflowPath, "utf8");
const marker = "          allow-ghsas: GHSA-qwww-vcr4-c8h2";
fs.writeFileSync(workflowPath, workflow.replace(marker, `${marker}\n          "warn-only": true`));
NODE
expect_failure "quoted additional dependency-review with key"

reset_fixture
node - "$fixture/.github/workflows/ci.yml" <<'NODE'
const fs = require("node:fs");
const workflowPath = process.argv[2];
const workflow = fs.readFileSync(workflowPath, "utf8");
fs.writeFileSync(workflowPath, workflow.replace("GHSA-qwww-vcr4-c8h2", "GHSA-xxxx-yyyy-zzzz"));
NODE
expect_failure "different advisory exception"

reset_fixture
node - "$fixture/.github/workflows/ci.yml" <<'NODE'
const fs = require("node:fs");
const workflowPath = process.argv[2];
const workflow = fs.readFileSync(workflowPath, "utf8");
fs.writeFileSync(
  workflowPath,
  workflow.replace("GHSA-qwww-vcr4-c8h2", "GHSA-qwww-vcr4-c8h2, GHSA-xxxx-yyyy-zzzz"),
);
NODE
expect_failure "additional advisory exception"

reset_fixture
cat >"$fixture/.github/workflows/ci.yml" <<'YAML'
jobs:
  npm-dependency-review:
    if: github.event_name == 'pull_request'
    steps:
      - uses: actions/checkout@example
        with:
          fail-on-severity: high
          allow-ghsas: GHSA-qwww-vcr4-c8h2
  gate:
    if: always()
    needs: [npm-dependency-review]
    steps:
      - name: Check results
        run: |
          if [[ "${{ github.event_name }}" == "pull_request" && "${{ needs.npm-dependency-review.result }}" != "success" ]]; then
            echo "::error::npm dependency review did not complete successfully"
            exit 1
          fi
YAML
expect_failure "advisory exception detached from dependency-review action"

reset_fixture
cat >"$fixture/.github/workflows/ci.yml" <<'YAML'
jobs:
  npm-dependency-review:
    if: github.event_name == 'pull_request'
    steps:
      - run: |
          echo "dependency review skipped"
          - uses: actions/dependency-review-action@3b139cfc5fae8b618d3eae3675e383bb1769c019
            with:
              fail-on-severity: high
              allow-ghsas: GHSA-qwww-vcr4-c8h2
  gate:
    if: always()
    needs: [npm-dependency-review]
    steps:
      - name: Check results
        run: |
          if [[ "${{ github.event_name }}" == "pull_request" && "${{ needs.npm-dependency-review.result }}" != "success" ]]; then
            echo "::error::npm dependency review did not complete successfully"
            exit 1
          fi
YAML
expect_failure "dependency-review action represented only by scalar text"

reset_fixture
node - "$fixture/.github/workflows/ci.yml" <<'NODE'
const fs = require("node:fs");
const workflowPath = process.argv[2];
const workflow = fs.readFileSync(workflowPath, "utf8");
fs.writeFileSync(
  workflowPath,
  workflow.replace(
    "        with:\n          fail-on-severity: high\n          allow-ghsas: GHSA-qwww-vcr4-c8h2",
    "        with: { fail-on-severity: critical, allow-ghsas: GHSA-qwww-vcr4-c8h2 }",
  ),
);
NODE
expect_failure "inline dependency-review inputs"

reset_fixture
node - "$fixture/.github/workflows/ci.yml" <<'NODE'
const fs = require("node:fs");
const workflowPath = process.argv[2];
const workflow = fs.readFileSync(workflowPath, "utf8");
fs.writeFileSync(
  workflowPath,
  workflow.replace(
    "needs: [npm-dependency-review, web-permission-tests]",
    "needs: [web-permission-tests]",
  ),
);
NODE
expect_failure "dependency-review action detached from required gate"

reset_fixture
node - "$fixture/.github/workflows/ci.yml" <<'NODE'
const fs = require("node:fs");
const workflowPath = process.argv[2];
const workflow = fs.readFileSync(workflowPath, "utf8");
fs.writeFileSync(workflowPath, workflow.replace("3b139cfc5fae8b618d3eae3675e383bb1769c019", "main"));
NODE
expect_failure "unpinned dependency-review action"

reset_fixture
node - "$fixture/.github/workflows/ci.yml" <<'NODE'
const fs = require("node:fs");
const workflowPath = process.argv[2];
const workflow = fs.readFileSync(workflowPath, "utf8");
fs.writeFileSync(workflowPath, workflow.replace("if: github.event_name == 'pull_request'", "if: false"));
NODE
expect_failure "skipped dependency-review job"

reset_fixture
node - "$fixture/.github/workflows/ci.yml" <<'NODE'
const fs = require("node:fs");
const workflowPath = process.argv[2];
const workflow = fs.readFileSync(workflowPath, "utf8");
fs.writeFileSync(
  workflowPath,
  workflow.replace(
    "      - uses: actions/dependency-review-action@3b139cfc5fae8b618d3eae3675e383bb1769c019",
    "      - uses: actions/dependency-review-action@3b139cfc5fae8b618d3eae3675e383bb1769c019\n        continue-on-error: true",
  ),
);
NODE
expect_failure "failure-tolerant dependency-review action"

reset_fixture
node - "$fixture/.github/workflows/ci.yml" <<'NODE'
const fs = require("node:fs");
const workflowPath = process.argv[2];
const workflow = fs.readFileSync(workflowPath, "utf8");
fs.writeFileSync(workflowPath, workflow.replace("fail-on-severity: high", "fail-on-severity: critical"));
NODE
expect_failure "weakened dependency-review severity"

reset_fixture
node - "$fixture/.github/workflows/ci.yml" <<'NODE'
const fs = require("node:fs");
const workflowPath = process.argv[2];
const workflow = fs.readFileSync(workflowPath, "utf8");
fs.writeFileSync(
  workflowPath,
  workflow.replace(
    "needs: [npm-dependency-review, web-permission-tests]",
    "needs: [npm-dependency-review-shadow, web-permission-tests]",
  ),
);
NODE
expect_failure "lookalike dependency-review gate need"

reset_fixture
node - "$fixture/.github/workflows/ci.yml" <<'NODE'
const fs = require("node:fs");
const workflowPath = process.argv[2];
const workflow = fs.readFileSync(workflowPath, "utf8");
fs.writeFileSync(workflowPath, workflow.replace('!= "success"', '== "success"'));
NODE
expect_failure "required gate without dependency-review success check"

reset_fixture
node - "$fixture/.github/workflows/ci.yml" <<'NODE'
const fs = require("node:fs");
const workflowPath = process.argv[2];
const workflow = fs.readFileSync(workflowPath, "utf8");
fs.writeFileSync(workflowPath, workflow.replace("    if: always()\n", ""));
NODE
expect_failure "required gate without always condition"

reset_fixture
node - "$fixture/.github/workflows/ci.yml" <<'NODE'
const fs = require("node:fs");
const workflowPath = process.argv[2];
const workflow = fs.readFileSync(workflowPath, "utf8");
fs.writeFileSync(
  workflowPath,
  workflow.replace("      - name: Check results", "      - name: Check results\n        continue-on-error: true"),
);
NODE
expect_failure "failure-tolerant required gate step"

reset_fixture
node - "$fixture/.github/workflows/ci.yml" <<'NODE'
const fs = require("node:fs");
const workflowPath = process.argv[2];
const workflow = fs.readFileSync(workflowPath, "utf8");
fs.writeFileSync(
  workflowPath,
  workflow.replace("      - name: Check results", "      - name: Check results\n        if: false"),
);
NODE
expect_failure "skipped required gate step"

reset_fixture
node - "$fixture/.github/workflows/ci.yml" <<'NODE'
const fs = require("node:fs");
const workflowPath = process.argv[2];
const workflow = fs.readFileSync(workflowPath, "utf8");
fs.writeFileSync(
  workflowPath,
  workflow.replace("        run: |\n          if [[", "        run: |\n          exit 0\n          if [["),
);
NODE
expect_failure "unreachable required gate assertion"

reset_fixture
cat >"$fixture/.github/workflows/ci.yml" <<'YAML'
jobs:
  npm-dependency-review:
    if: github.event_name == 'pull_request'
    steps:
      - uses: actions/dependency-review-action@3b139cfc5fae8b618d3eae3675e383bb1769c019
        with:
          fail-on-severity: high
          allow-ghsas: GHSA-qwww-vcr4-c8h2
  gate:
    if: always()
    needs: [npm-dependency-review]
    steps:
      - name: Harmless step
        run: |
          exit 0
          - name: Check results
            run: |
              if [[ "${{ github.event_name }}" == "pull_request" && "${{ needs.npm-dependency-review.result }}" != "success" ]]; then
                echo "::error::npm dependency review did not complete successfully"
                exit 1
              fi
YAML
expect_failure "required gate represented only by scalar text"

reset_fixture
node - "$fixture/.github/workflows/ci.yml" <<'NODE'
const fs = require("node:fs");
const workflowPath = process.argv[2];
const workflow = fs.readFileSync(workflowPath, "utf8");
fs.writeFileSync(
  workflowPath,
  workflow.replace("      - name: Check results", "      - name: Check results\n        shell: bash -n {0}"),
);
NODE
expect_failure "required gate shell override"

reset_fixture
node - "$fixture/.github/workflows/ci.yml" <<'NODE'
const fs = require("node:fs");
const workflowPath = process.argv[2];
const workflow = fs.readFileSync(workflowPath, "utf8");
fs.writeFileSync(workflowPath, `defaults:\n  run:\n    shell: bash -n {0}\n${workflow}`);
NODE
expect_failure "workflow-wide shell override"

reset_fixture
node - "$fixture/.github/workflows/ci.yml" <<'NODE'
const fs = require("node:fs");
const workflowPath = process.argv[2];
const workflow = fs.readFileSync(workflowPath, "utf8");
fs.writeFileSync(workflowPath, workflow.replace("run: npm run test:rsc-guard", "run: npm run test:contexts"));
NODE
expect_failure "missing web semantic guard step"

reset_fixture
node - "$fixture/.github/workflows/ci.yml" <<'NODE'
const fs = require("node:fs");
const workflowPath = process.argv[2];
const workflow = fs.readFileSync(workflowPath, "utf8");
fs.writeFileSync(
  workflowPath,
  workflow.replace(
    "needs: [npm-dependency-review, web-permission-tests]",
    "needs: [npm-dependency-review]",
  ),
);
NODE
expect_failure "web semantic guard detached from required gate"

reset_fixture
node - "$fixture/.github/workflows/ci.yml" <<'NODE'
const fs = require("node:fs");
const workflowPath = process.argv[2];
const workflow = fs.readFileSync(workflowPath, "utf8");
fs.writeFileSync(
  workflowPath,
  workflow.replace("      - name: Check results", "      - name: Check results\n        \"continue-on-error\": true"),
);
NODE
expect_failure "quoted failure-tolerant required gate field"

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
node - "$fixture/web/package.json" <<'NODE'
const fs = require("node:fs");
const packagePath = process.argv[2];
const pkg = JSON.parse(fs.readFileSync(packagePath, "utf8"));
pkg.dependencies.rr = "npm:react-router@7.18.2";
fs.writeFileSync(packagePath, `${JSON.stringify(pkg, null, 2)}\n`);
NODE
expect_failure "npm alias for react-router"

reset_fixture
node - "$fixture/web/package.json" <<'NODE'
const fs = require("node:fs");
const packagePath = process.argv[2];
const pkg = JSON.parse(fs.readFileSync(packagePath, "utf8"));
pkg.dependencies.rr = "npm:react-router-dom@7.18.2";
fs.writeFileSync(packagePath, `${JSON.stringify(pkg, null, 2)}\n`);
NODE
expect_failure "npm alias for react-router-dom"

for protocol in file link workspace; do
  reset_fixture
  node - "$fixture/web/package.json" "$protocol" <<'NODE'
const fs = require("node:fs");
const packagePath = process.argv[2];
const protocol = process.argv[3];
const pkg = JSON.parse(fs.readFileSync(packagePath, "utf8"));
pkg.dependencies["local-page"] = `${protocol}:../local-page`;
fs.writeFileSync(packagePath, `${JSON.stringify(pkg, null, 2)}\n`);
NODE
  expect_failure "$protocol dependency protocol"
done

reset_fixture
node - "$fixture/web/package.json" <<'NODE'
const fs = require("node:fs");
const packagePath = process.argv[2];
const pkg = JSON.parse(fs.readFileSync(packagePath, "utf8"));
delete pkg.dependencies["react-router-dom"];
fs.writeFileSync(packagePath, `${JSON.stringify(pkg, null, 2)}\n`);
NODE
expect_failure "missing direct react-router-dom dependency"

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

echo "web-rsc-mode-guard policy tests passed"
