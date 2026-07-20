# SLSA Provenance Attestation

**Last updated:** 2026-07-20

**Related:** Issue #9101, PR #8277, RFC #8177

**Scope:** Release pipeline (`release-stable-manual.yml`, `publish` job)

## Canonical model

Downloadable release assets use GitHub artifact attestations as their single
provenance mechanism. `actions/attest-build-provenance` records SLSA v1.0
Build Level 2 provenance in GitHub's artifact-attestation API.

A release page produced by the consolidated workflow contains the payloads,
`SHA256SUMS`, both SBOM formats, and at most one
`zeroclaw-vX.Y.Z-verification.tar.gz` archive. It does not contain per-asset
cosign bundles, loose attestation bundles, or generic SLSA-generator
`.intoto.jsonl` files. Cosign remains in the release workflow only for GHCR
container-image signing.

The verification archive contains:

- one offline GitHub attestation bundle for every payload present before the
  archive is created;
- GitHub and Sigstore trusted-root material;
- an artifact-to-bundle index with SHA-256 digests.

After the archive is created, the workflow generates final `SHA256SUMS`, which
includes the archive, and separately attests both metadata files. The archive
cannot contain its own attestation bundle because doing so would change the
digest being attested. Users therefore verify the archive and `SHA256SUMS`
online before moving the payload and extracted bundle into an offline
environment.

## What provenance establishes

A successful verification establishes that:

- the local file matches the digest in a signed attestation;
- the attestation was issued for the ZeroClaw repository;
- `release-stable-manual.yml` was the signer workflow;
- the attestation names the expected source commit.

It establishes build origin and recorded build instructions. It does not
establish source quality, review approval, or artifact safety.

## Threat model

| Threat | Covered? | Explanation |
|---|---|---|
| Release-page write access swaps a payload | Yes | The substituted file does not match the attested digest. |
| A local build is presented as an official CI build | Yes | The signer workflow and source digest checks fail. |
| A developer machine is compromised | Partial | A locally built substitute fails verification, but stolen maintainer credentials may authorize repository changes. |
| Malicious or vulnerable dependency | No | Provenance records origin; SBOM and vulnerability analysis address content. |
| Compromised GitHub-hosted runner | No | The runner can alter inputs before the attestation step. |
| Compromised maintainer account | No | An authorized account can change source or workflow instructions. |
| GitHub OIDC or control-plane compromise | No | GitHub is the root of trust for signing and hosted attestation storage. |
| Missing Phase A attestation | No | Best-effort failure is disclosed in the release notes; consumers must not treat absence as success. |

## Verification paths

Online verification obtains the bundle and trust material from GitHub:

```bash
gh attestation verify <artifact> \
  --repo zeroclaw-labs/zeroclaw \
  --signer-workflow zeroclaw-labs/zeroclaw/.github/workflows/release-stable-manual.yml \
  --source-digest <release-commit-sha>
```

Offline verification uses the extracted bundle and trusted root:

```bash
gh attestation verify <artifact> \
  --repo zeroclaw-labs/zeroclaw \
  --signer-workflow zeroclaw-labs/zeroclaw/.github/workflows/release-stable-manual.yml \
  --source-digest <release-commit-sha> \
  --bundle verification/<artifact>.attestation.jsonl \
  --custom-trusted-root verification/trusted_root.jsonl
```

The exact connected-staging and disconnected commands live in
[`release-verification.md`](../book/src/maintainers/release-verification.md).

## Rollout phase

Attestation generation and offline-bundle packaging remain best-effort Phase A
operations (`continue-on-error: true`). This consolidation does not decide the
Phase B hard-gate or hardware/offline-signing roadmap from RFC #8177. A release
whose provenance or archive step fails remains publishable, but its release
notes must disclose the gap and link to the workflow run.

## References

- [GitHub artifact attestations](https://docs.github.com/en/actions/security-guides/using-artifact-attestations)
- [GitHub CLI attestation verification](https://cli.github.com/manual/gh_attestation_verify)
- [SLSA specification](https://slsa.dev/spec/v1.0/levels)
