#!/usr/bin/env bash
set -euo pipefail

# This harness deliberately uses the same matcher dependency as the pinned
# actions/labeler release. Keep the versions together when that action moves.
readonly ACTION_REF='actions/labeler@bf12e9b00b37c5c0ca2b87b79b2daf7891dbda13'
readonly ACTION_VERSION='v7.0.0'
readonly MINIMATCH_VERSION='10.2.5'
readonly JS_YAML_VERSION='5.1.0'

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

npm install --prefix "$tmp_dir" --no-save --ignore-scripts --package-lock=false \
  "minimatch@${MINIMATCH_VERSION}" "js-yaml@${JS_YAML_VERSION}" >/dev/null

REPO_ROOT="$repo_root" NODE_PATH="$tmp_dir/node_modules" \
  PATH_LABELER_ACTION_REF="$ACTION_REF" PATH_LABELER_ACTION_VERSION="$ACTION_VERSION" \
  PATH_LABELER_MINIMATCH_VERSION="$MINIMATCH_VERSION" node <<'NODE'
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const { execFileSync } = require('node:child_process');
const yaml = require('js-yaml');
const { Minimatch } = require('minimatch');

const root = process.env.REPO_ROOT;
const labelerPath = path.join(root, '.github', 'labeler.yml');
const workflowPath = path.join(root, '.github', 'workflows', 'pr-path-labeler.yml');
const config = yaml.load(fs.readFileSync(labelerPath, 'utf8'));
const workflow = fs.readFileSync(workflowPath, 'utf8');
const trackedFiles = new Set(
  execFileSync('git', ['-C', root, 'ls-files', '-z'], { encoding: 'utf8' })
    .split('\0')
    .filter(Boolean),
);

assert.match(workflow, new RegExp(`${process.env.PATH_LABELER_ACTION_REF} \\# ${process.env.PATH_LABELER_ACTION_VERSION}`));

function matchesLabel(label, files) {
  const changedFiles = (config[label] || []).flatMap((entry) => entry['changed-files'] || []);
  return changedFiles.some((rule) => {
    const patterns = rule['any-glob-to-any-file'] || [];
    return patterns.some((pattern) => {
      const matcher = new Minimatch(pattern, { dot: true });
      return files.some((file) => matcher.match(file));
    });
  });
}

const positives = [
  ['cli', 'src/memory/cli.rs'],
  ['cli', 'src/commands/update.rs'],
  ['hardware', 'src/peripherals/mod.rs'],
  ['hardware', 'crates/zeroclaw-api/src/peripherals_traits.rs'],
  ['hardware', 'firmware/esp32-ui/src/main.rs'],
];
const negatives = [
  ['cli', 'src/memory/mod.rs'],
  ['cli', 'crates/zeroclaw-memory/src/cli.rs'],
  ['hardware', 'src/peripherals/driver.rs'],
  ['hardware', 'crates/zeroclaw-api/src/channel.rs'],
  ['hardware', 'crates/zeroclaw-runtime/src/agent/history.rs'],
  ['hardware', 'docs/book/src/hardware/subsystem.md'],
];

for (const [label, file] of positives) {
  assert.equal(trackedFiles.has(file), true, `${file} must be a tracked repository path`);
  assert.equal(matchesLabel(label, [file]), true, `${label} should match ${file}`);
}
for (const [label, file] of negatives) {
  assert.equal(matchesLabel(label, [file]), false, `${label} should not match ${file}`);
}

console.log(`Pinned matcher: minimatch ${process.env.PATH_LABELER_MINIMATCH_VERSION} (${process.env.PATH_LABELER_ACTION_REF} # ${process.env.PATH_LABELER_ACTION_VERSION})`);
console.log(`${positives.length}/${positives.length} configured positives matched.`);
console.log(`${negatives.length}/${negatives.length} boundary negatives stayed unmatched.`);
NODE
