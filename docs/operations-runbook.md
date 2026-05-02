# ZeroClaw Operations Runbook

This runbook is for operators who maintain availability, security posture, and incident response.

Last verified: **February 18, 2026**.

## Scope

Use this document for day-2 operations:

- starting and supervising runtime
- health checks and diagnostics
- safe rollout and rollback
- incident triage and recovery

For first-time installation, start from [one-click-bootstrap.md](one-click-bootstrap.md).

## Runtime Modes

| Mode | Command | When to use |
|---|---|---|
| Foreground runtime | `zeroclaw daemon` | local debugging, short-lived sessions |
| Foreground gateway only | `zeroclaw gateway` | webhook endpoint testing |
| User service | `zeroclaw service install && zeroclaw service start` | persistent operator-managed runtime |

## Baseline Operator Checklist

1. Validate configuration:

```bash
zeroclaw status
```

2. Verify diagnostics:

```bash
zeroclaw doctor
zeroclaw channel doctor
```

3. Start runtime:

```bash
zeroclaw daemon
```

4. For persistent user session service:

```bash
zeroclaw service install
zeroclaw service start
zeroclaw service status
```

## Health and State Signals

| Signal | Command / File | Expected |
|---|---|---|
| Config validity | `zeroclaw doctor` | no critical errors |
| Channel connectivity | `zeroclaw channel doctor` | configured channels healthy |
| Runtime summary | `zeroclaw status` | expected provider/model/channels |
| Daemon heartbeat/state | `~/.zeroclaw/daemon_state.json` | file updates periodically |

## Logs and Diagnostics

### macOS / Windows (service wrapper logs)

- `~/.zeroclaw/logs/daemon.stdout.log`
- `~/.zeroclaw/logs/daemon.stderr.log`

### Linux (systemd user service)

```bash
journalctl --user -u zeroclaw.service -f
```

## Incident Triage Flow (Fast Path)

1. Snapshot system state:

```bash
zeroclaw status
zeroclaw doctor
zeroclaw channel doctor
```

2. Check service state:

```bash
zeroclaw service status
```

3. If service is unhealthy, restart cleanly:

```bash
zeroclaw service stop
zeroclaw service start
```

4. If channels still fail, verify allowlists and credentials in `~/.zeroclaw/config.toml`.

5. If gateway is involved, verify bind/auth settings (`[gateway]`) and local reachability.

## Secret Leak Incident Response (CI Gitleaks)

When `sec-audit.yml` reports a gitleaks finding or uploads SARIF alerts:

1. Confirm whether the finding is a true credential leak or a test/doc false positive:
   - review `gitleaks.sarif` + `gitleaks-summary.json` artifacts
   - inspect changed commit range in the workflow summary
2. If true positive:
   - revoke/rotate the exposed secret immediately
   - remove leaked material from reachable history when required by policy
   - open an incident record and track remediation ownership
3. If false positive:
   - prefer narrowing detection scope first
   - only add allowlist entries with explicit governance metadata (`owner`, `reason`, `ticket`, `expires_on`)
   - ensure the related governance ticket is linked in the PR
4. Re-run `Sec Audit` and confirm:
   - gitleaks lane green
   - governance guard green
   - SARIF upload succeeds

## Safe Change Procedure

Before applying config changes:

1. backup `~/.zeroclaw/config.toml`
2. apply one logical change at a time
3. run `zeroclaw doctor`
4. restart daemon/service
5. verify with `status` + `channel doctor`

## Rollback Procedure

If a rollout regresses behavior:

1. restore previous `config.toml`
2. restart runtime (`daemon` or `service`)
3. confirm recovery via `doctor` and channel health checks
4. document incident root cause and mitigation

## Domain DB Publication (lawpro / medpro forks only)

> **General-public MoA does not publish a domain corpus.** The
> following procedures apply to operators of specialized forks
> (lawpro for Korean legal data, future medpro for medical, etc.)
> who run their own R2/S3 bucket. Full protocol and rationale:
> [`docs/domain-db-incremental-design.md`](domain-db-incremental-design.md).

The protocol has two cadences: **annual baseline** (default: every
January 15) and **occasional delta** (whenever the corpus actually
changes — sometimes weekly, sometimes silent for a month). Clients
poll on a fixed weekly schedule and resolve to one of FullInstall,
AlreadyCurrent (zero bytes), or ApplyDelta. The protocol accepts
"operator was silent for six weeks" as the normal case.

### Annual Baseline Cut (once per year)

```bash
# 1. Build the full corpus into a fresh DB.
python scripts/build_domain_db_fast.py \
       --corpus-dir corpus/legal \
       --out  out/korean-legal-2026.01.15.db

# 2. Stamp the baseline meta into the DB itself.
zeroclaw vault domain stamp-baseline \
       --db   out/korean-legal-2026.01.15.db \
       --version 2026.01.15

# 3. Emit a v2 manifest with `deltas: []`.
zeroclaw vault domain publish-v2 \
       --baseline out/korean-legal-2026.01.15.db \
       --baseline-url https://r2.example.com/moa/domain/korean-legal-2026.01.15.db \
       --name korean-legal \
       --baseline-version 2026.01.15 \
       --out-manifest out/korean-legal.manifest.json

# 4. Upload both files. The bundle URL must match what the manifest declared.
aws s3 cp out/korean-legal-2026.01.15.db \
          s3://moa-domain/korean-legal-2026.01.15.db
aws s3 cp out/korean-legal.manifest.json \
          s3://moa-domain/korean-legal.manifest.json
```

After upload, every existing client's next weekly poll hits
`FullInstall` and re-downloads the new baseline. Schema-breaking
changes (new vault columns) ride on this cut — mid-year deltas
keep the same schema as the in-place baseline.

### Periodic Delta Publication (whenever corpus changes)

```bash
# 1. Build a fresh full DB from the latest corpus.
python scripts/build_domain_db_fast.py \
       --corpus-dir corpus/legal \
       --out  out/staging-2026.04.22.db

# 2. Diff against the published baseline → emit cumulative delta.
python scripts/build_domain_delta.py \
       --baseline out/korean-legal-2026.01.15.db \
       --current  out/staging-2026.04.22.db \
       --out      out/korean-legal-delta-2026.04.22.sqlite \
       --version  2026.04.22 \
       --applies-to-baseline 2026.01.15

# 3. Append the delta to the live manifest. The CLI re-validates
#    `applies_to_baseline`, computes sha256 + size, refuses to add
#    a duplicate version, and bumps the manifest's chain head.
zeroclaw vault domain publish-delta \
       --delta out/korean-legal-delta-2026.04.22.sqlite \
       --delta-url https://r2.example.com/moa/domain/korean-legal-delta-2026.04.22.sqlite \
       --delta-version 2026.04.22 \
       --in-manifest  out/korean-legal.manifest.json \
       --out-manifest out/korean-legal.manifest.json

# 4. Upload the delta + the updated manifest.
aws s3 cp out/korean-legal-delta-2026.04.22.sqlite \
          s3://moa-domain/korean-legal-delta-2026.04.22.sqlite
aws s3 cp out/korean-legal.manifest.json \
          s3://moa-domain/korean-legal.manifest.json
```

**Cumulative, not chained**: every `build_domain_delta.py` run diffs
against the same baseline (not against the previous delta). A client
that's 11 weeks behind downloads exactly the most recent delta and
catches up in one shot.

### Silent Weeks

The operator does nothing. The previous manifest stays on R2.
Clients fetch ~1 KB of JSON, hit `AlreadyCurrent`, download zero
bytes more. This is the protocol's common case — design accordingly.

### Pruning Old Deltas

Clients only ever read the last entry in `manifest.deltas` (cumulative
semantics). Older delta files on R2 can be deleted once a newer one is
published; we recommend retaining the previous delta for ~7 days as a
rollback safety net. Trim `manifest.deltas` to `[latest]` only when you
also delete the older bucket objects, otherwise old clients get a 404.

### Verifying a Publication

```bash
# Round-trip the published manifest through the validator.
zeroclaw vault domain info   # dumps installed state + last_applied_at
# A test client (separate workspace) should: fetch → AlreadyCurrent
# on rerun, ApplyDelta on a fresh install once a new delta lands.
```

### Rollback Across the Update Boundary

If a delta corrupts a class of cases:

1. Locally regenerate the delta (`build_domain_delta.py`) with the
   buggy rows excluded.
2. `publish-delta` with the same `--delta-version` will refuse
   ("already present"). Bump the version (`2026.04.22b`) and republish.
3. Clients on a polling cycle pick up the corrected chain head on
   their next wake-up.

For a full retreat, restore the prior `manifest.json` from your
operator's backup and re-upload — the live deltas in R2 are
unchanged, only the chain head flips back.

## Related Docs

- [one-click-bootstrap.md](one-click-bootstrap.md)
- [troubleshooting.md](troubleshooting.md)
- [config-reference.md](config-reference.md)
- [commands-reference.md](commands-reference.md)
- [domain-db-incremental-design.md](domain-db-incremental-design.md) — full design
