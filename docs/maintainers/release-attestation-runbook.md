# Release Runbook: Artifact Attestations

**File:** `.github/workflows/release-stable-manual.yml`, `publish` job

**Policy:** best-effort Phase A (`continue-on-error: true`)

**Canonical downloadable-asset mechanism:** GitHub artifact attestations

## Expected sequence

The `publish` job performs these operations in order:

1. generate SPDX and CycloneDX SBOMs;
2. collect every release payload, including `install.sh` and both SBOMs;
3. attest those payloads through GitHub;
4. download and locally verify each offline bundle;
5. package the bundles, trusted root, and index into one verification archive;
6. generate final `SHA256SUMS`, including the verification archive;
7. attest `SHA256SUMS` and the verification archive;
8. create the GitHub Release from `release-assets/*`.

Do not move checksum generation before archive creation or edit
`SHA256SUMS` after its attestation. Either change produces stale metadata.

## Failure symptoms

| Failed step | Expected release behavior | Consumer impact |
|---|---|---|
| `Attest release payloads` | Release continues | No downloadable payload has trusted provenance; release notes disclose the gap. |
| `Package offline verification archive` | Release continues | Online payload verification remains available; no offline archive is published. |
| `Attest final checksums` | Release continues | Payloads remain verifiable, but release notes do not advertise the offline bootstrap path. |
| `Attest verification archive` | Release continues | The archive may be present but release notes do not advertise it for trusted offline staging. |
| Either SBOM generation step | Release stops before publication | Neither partial SBOM coverage nor a partially assembled release is published. |

For HTTP 401, 429, 5xx, OIDC-token, or GitHub API failures, check
[GitHub Status](https://www.githubstatus.com/) and retry the workflow once the
service is healthy. A missing `id-token: write` or `attestations: write`
permission is a workflow regression, not a transient failure.

Attestations require the GitHub-issued OIDC token from the workflow run. They
cannot be recreated later from a maintainer workstation.

## Required release rehearsal

The first release after changing this contract requires a human maintainer to
complete this checklist before issue #9101 is closed:

```text
[ ] Release contains both SBOM formats.
[ ] Release contains SHA256SUMS and exactly one *-verification.tar.gz.
[ ] Release contains no *.bundle, loose *.attestation.jsonl, or *.intoto.jsonl assets.
[ ] SHA256SUMS contains the verification archive and all other release assets.
[ ] gh attestation verify succeeds online for a binary, install.sh, one SBOM,
    SHA256SUMS, and the verification archive.
[ ] The archive contains trusted_root.jsonl, ATTESTATION-BUNDLES.md, and one
    bundle for every payload listed in its index.
[ ] With network access disabled, gh attestation verify succeeds for the
    spot-checked binary using only its extracted bundle and trusted root.
[ ] Both GHCR image variants remain cosign-verifiable by immutable digest.
[ ] Release notes state the Build Level 2 claim and threat-model limits.
```

Record the release tag, workflow-run URL, exact commands, and redacted output in
the implementing PR. Do not claim the rehearsal is complete based only on
workflow linting, a dry run, or a previous release.

## Manual inspection commands

```bash
TAG=vX.Y.Z
SOURCE_DIGEST=<release-commit-sha>
VERIFY_ARCHIVE="zeroclaw-${TAG}-verification.tar.gz"

gh release view "$TAG" --repo zeroclaw-labs/zeroclaw \
  --json assets --jq '.assets[].name'
gh release download "$TAG" --repo zeroclaw-labs/zeroclaw \
  --pattern SHA256SUMS --pattern "$VERIFY_ARCHIVE"
awk -v file="$VERIFY_ARCHIVE" '$2 == file { print }' SHA256SUMS | sha256sum -c -
gh attestation verify "$VERIFY_ARCHIVE" \
  --repo zeroclaw-labs/zeroclaw \
  --signer-workflow zeroclaw-labs/zeroclaw/.github/workflows/release-stable-manual.yml \
  --source-digest "$SOURCE_DIGEST"
tar -tzf "$VERIFY_ARCHIVE"
```

The complete consumer commands are maintained in
`docs/book/src/maintainers/release-verification.md` and must be tested
verbatim during the rehearsal.
