# CI Runner Incident Report: main branch red on 2026-03-02

This report is for CI runner maintainers to debug runner health regressions first, before restoring self-hosted execution for critical workflows.

## Scope

- Repo: `zeroclaw-labs/zeroclaw`
- Date window: 2026-03-02 (UTC)
- Impacted checks:
  - `CI Supply Chain Provenance / Build + Provenance Bundle (push)`
  - `Test E2E / Integration / E2E Tests (push)`

## Executive Summary

`main` became red due to runner-environment failures in self-hosted pools.

Observed failure classes:

1. Missing C compiler linker (`cc`) causing Rust build-script compile failures.
2. Disk exhaustion (`No space left on device`) on at least one self-hosted E2E run.

These are host-level failures and were reproduced across unrelated merge commits.

## Evidence

| Time (UTC) | Workflow run | Commit | Runner | Failure signature |
|---|---|---|---|---|
| 2026-03-02T02:04:42Z | https://github.com/zeroclaw-labs/zeroclaw/actions/runs/22558446611 | `4b16ac92197d98bd64a43ae750d473b9f1c6d66d` | `runner-a` (`self-hosted-pool`) | `error: linker 'cc' not found` + `No such file or directory (os error 2)` |
| 2026-03-02T02:04:42Z | https://github.com/zeroclaw-labs/zeroclaw/actions/runs/22558446636 | `4b16ac92197d98bd64a43ae750d473b9f1c6d66d` | `runner-b` (`self-hosted-pool`) | `error: linker 'cc' not found` + `No such file or directory (os error 2)` |
| 2026-03-02T01:54:26Z | https://github.com/zeroclaw-labs/zeroclaw/actions/runs/22558247107 | `b8e5707d180004fe00fa12bfacd1bcf29f195457` | `runner-c` (`self-hosted-pool`) | `error: linker 'cc' not found` + `No such file or directory (os error 2)` |
| 2026-03-02T01:25:15Z | https://github.com/zeroclaw-labs/zeroclaw/actions/runs/22557668884 | `64a2a271c74fc84276e98231196b749f29276d17` | `runner-d` (`self-hosted-pool`) | `error: linker 'cc' not found` + `No such file or directory (os error 2)` |
| 2026-03-02T01:25:15Z | https://github.com/zeroclaw-labs/zeroclaw/actions/runs/22557668895 | `64a2a271c74fc84276e98231196b749f29276d17` | `runner-e` (`self-hosted-pool`) | `No space left on device` |

## Why this is runner infra

- Same `cc` failure appears in multiple independent merges.
- Failure happens within ~11-15 seconds during bootstrap/compile stage.
- Similar test lane succeeded in nearby window on a different runner host, indicating host drift rather than deterministic code break.

## Debug Procedure (Runner Maintainers)

Run on each affected host and attach outputs to incident ticket.

```bash
# identity
hostname
uname -a

# required build toolchain
command -v cc || true
command -v gcc || true
command -v clang || true
command -v rustc || true
command -v cargo || true
ls -l /usr/bin/cc || true

# versions
cc --version || true
gcc --version | head -n1 || true
clang --version | head -n1 || true
rustc --version || true
cargo --version || true

# disk and inode pressure
df -h /
df -h /opt/actions-runners || true
df -Pi /
df -Pi /opt/actions-runners || true

# top disk consumers
du -h /opt/actions-runners --max-depth=2 2>/dev/null | sort -h | tail -n 40

# runner service logs (service name may vary)
sudo journalctl -u actions.runner\* --since "2026-03-02 00:00:00" -n 300 --no-pager || true
```

If `cc` is missing:

```bash
sudo apt-get update
sudo apt-get install -y build-essential pkg-config clang
command -v cc || sudo ln -sf /usr/bin/gcc /usr/bin/cc
cc --version
```

If disk is low / inode pressure is high:

```bash
sudo du -h /opt/actions-runners --max-depth=3 | sort -h | tail -n 60
# clean stale _work/_temp/_diag artifacts per runner ops policy
```

## Mitigation Applied in This PR

1. Immediate unblock on `main`:
   - `test-e2e.yml` moved to `ubuntu-22.04`.
   - `ci-supply-chain-provenance.yml` moved to `ubuntu-22.04`.
2. Preflight hardening:
   - added explicit checks for `cc` and free disk (>=10 GiB) in those jobs.
3. Root-cause visibility:
   - `test-self-hosted.yml` now includes compiler + disk/inode checks and daily schedule.

## Exit Criteria to move lanes back to self-hosted

1. Self-hosted health workflow passes on representative nodes.
2. 10 consecutive critical runs pass on self-hosted without `cc` or ENOSPC failures.
3. Runner image baseline explicitly includes compiler/runtime prerequisites and cleanup policy.
4. Health checks remain stable for 24h after rollback from hosted fallback.
