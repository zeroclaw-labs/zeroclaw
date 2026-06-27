# Release Runbook — Attestation Step Failure

**File:** `.github/workflows/release-stable-manual.yml` — `publish` job  
**Step:** `Attach SLSA provenance attestation`  
**Policy:** `continue-on-error: true` (Phase A — best-effort)

---

## Symptoms

- The `Attach SLSA provenance attestation` step logs a failure
- The `Download attestation bundles for offline verification` step also fails (depends on first step)
- The `Create tag and release` step runs anyway — release is published but **without attestation bundles**
- Release notes still include verification instructions, but verification will fail because no attestation exists

---

## Immediate Triage

Read the step log. Match the error:

| Error Pattern | Likely Cause | Action |
|---|---|---|
| `401 Unauthorized` | OIDC token issue — transient | Retry the workflow. If it passes, done. |
| `429 Too Many Requests` | GitHub API rate limit — burst | Wait 5 minutes, retry. |
| `500 Internal Server Error` | GitHub API outage | Check [status.github.com](https://status.github.com). Retry when green. |
| `404 Not Found` | Subject path glob matched nothing | Check `release-assets/` directory contents. File names may have changed. |
| `could not parse OIDC token` | Runner identity issue | Check `id-token: write` permission is still on the `publish` job. |
| Timeout (>260s) | Network issue or GH API slow | Retry. If persistent, check runner connectivity. |

---

## Decision Matrix

| Scenario | Action | Escalation? |
|---|---|---|
| First failure in weeks | Retry the workflow | No |
| 2-3 failures in a row | Retry once. If still fails, check status.github.com | No |
| 3+ consecutive releases failing | Investigate permissions or action version change | File a security issue with collected evidence |
| Every release fails since deploy | Bug in the workflow change — revert the attestation step | Page author |

---

## How to Retry

1. Go to the failed workflow run
2. Click **Re-run jobs** → **Re-run failed jobs**
3. Only the `publish` job re-runs — build artifacts are preserved

If the attestation step passes but the download bundles step fails:
- The release still has attestations in GitHub's API
- Only offline verification via `.attestation` bundles is affected
- No need to re-release

---

## If All Else Fails — Manual Remediation

If the release was published without attestations and you need to add them:

```bash
# 1. Download the release artifacts locally
gh release download <tag> --dir release-assets/

# 2. Generate attestation manually (requires OIDC token — must run in GH Actions)
# This cannot be done outside GitHub Actions. Instead:

# 3. Document the gap in the release notes
gh release edit <tag> --notes-file - <<EOF
NOTE: SLSA attestation is unavailable for this release due to a
transient pipeline failure. See PR #8277 for context.
EOF
```

**Key constraint:** Attestation generation requires GitHub's OIDC token, which only exists inside a workflow run. You cannot retroactively generate attestations from a local machine.

---

## Prevention

- Consider promoting to Phase B (`continue-on-error: false` + alert on failure)
  once Phase A has stabilized
- Monitor attestation step duration — sudden slowdowns precede outages
- Keep `actions/attest-build-provenance` version pinned (already done)
