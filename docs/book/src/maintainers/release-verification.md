# Release Artifact Verification

ZeroClaw uses GitHub artifact attestations as the canonical provenance mechanism
for downloadable release assets. Each successful attestation records the asset
digest, source commit, and release workflow identity as SLSA v1.0 Build Level 2
provenance.

Attestation proves where and how an artifact was built. It does not prove that a
human reviewed the source, that dependencies are safe, or that a maintainer
account, GitHub-hosted runner, or GitHub control plane was not compromised. See
[SLSA provenance attestation](../../../security/slsa-provenance.md) for the full
threat model.

The release workflow currently treats attestations as best-effort Phase A
output. Check the release notes before relying on verification material; they
state whether online provenance and the offline archive were produced.

## Online verification

Install the [GitHub CLI](https://cli.github.com/), download an asset, and verify
it against the release workflow and source commit:

```bash
VERSION=vX.Y.Z
SOURCE_DIGEST=<40-character-release-commit>
ASSET=zeroclaw-x86_64-unknown-linux-gnu.tar.gz

gh release download "$VERSION" --repo zeroclaw-labs/zeroclaw \
  --pattern "$ASSET"
gh attestation verify "$ASSET" \
  --repo zeroclaw-labs/zeroclaw \
  --signer-workflow zeroclaw-labs/zeroclaw/.github/workflows/release-stable-manual.yml \
  --source-digest "$SOURCE_DIGEST"
```

Use the full commit shown for the release tag. A successful command prints the
verified attestation and subject digest. The same command applies to
`install.sh`, `SHA256SUMS`, both SBOM files, and the verification archive.

## Offline verification

A release produced by the consolidated workflow with complete Phase A output
publishes one archive named
`zeroclaw-vX.Y.Z-verification.tar.gz`. It contains:

- one `<artifact>.attestation.jsonl` bundle for each release payload;
- `trusted_root.jsonl`, the GitHub and Sigstore trusted-root material;
- `ATTESTATION-BUNDLES.md`, an index of artifact names, SHA-256 digests, and
  bundle names.

The archive cannot contain its own attestation without creating a circular
digest. Bootstrap trust while connected by verifying the archive and final
checksum file online. Then transfer the artifact, archive, and checksum file to
the offline environment.

### Connected staging step

```bash
VERSION=vX.Y.Z
SOURCE_DIGEST=<40-character-release-commit>
ASSET=zeroclaw-x86_64-unknown-linux-gnu.tar.gz
VERIFY_ARCHIVE="zeroclaw-${VERSION}-verification.tar.gz"

gh release download "$VERSION" --repo zeroclaw-labs/zeroclaw \
  --pattern "$ASSET" \
  --pattern SHA256SUMS \
  --pattern "$VERIFY_ARCHIVE"

gh attestation verify "$VERIFY_ARCHIVE" \
  --repo zeroclaw-labs/zeroclaw \
  --signer-workflow zeroclaw-labs/zeroclaw/.github/workflows/release-stable-manual.yml \
  --source-digest "$SOURCE_DIGEST"
gh attestation verify SHA256SUMS \
  --repo zeroclaw-labs/zeroclaw \
  --signer-workflow zeroclaw-labs/zeroclaw/.github/workflows/release-stable-manual.yml \
  --source-digest "$SOURCE_DIGEST"

awk -v file="$VERIFY_ARCHIVE" '$2 == file { print }' SHA256SUMS | sha256sum -c -
mkdir verification
tar -xzf "$VERIFY_ARCHIVE" -C verification
```

Also compare the artifact digest with its row in `SHA256SUMS` before transfer:

```bash
awk -v file="$ASSET" '$2 == file { print }' SHA256SUMS | sha256sum -c -
```

### Disconnected verification step

No network request is required when both `--bundle` and
`--custom-trusted-root` point to the staged verification material:

```bash
gh attestation verify "$ASSET" \
  --repo zeroclaw-labs/zeroclaw \
  --signer-workflow zeroclaw-labs/zeroclaw/.github/workflows/release-stable-manual.yml \
  --source-digest "$SOURCE_DIGEST" \
  --bundle "verification/${ASSET}.attestation.jsonl" \
  --custom-trusted-root verification/trusted_root.jsonl
```

`SHA256SUMS` and the verification archive are online-bootstrap metadata and do
not have bundles inside the archive. All payloads present before the archive is
built, including `install.sh` and both SBOMs, do.

## SBOMs

Two checksummed and attested SBOM files are published with each release:

| File | Format |
|---|---|
| `zeroclaw-vX.Y.Z-sbom.spdx.json` | SPDX JSON |
| `zeroclaw-vX.Y.Z-sbom.cdx.json` | CycloneDX JSON |

Verify either SBOM with the same online or offline attestation command used for
a binary asset. Tools such as Syft or Grype can then inspect the verified file.

## Container images

GHCR container images remain signed by digest with cosign. This is independent
of the GitHub-attestation path for downloadable release assets.

```bash
IMAGE=ghcr.io/zeroclaw-labs/zeroclaw
TAG=vX.Y.Z

cosign verify \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-identity-regexp "^https://github.com/zeroclaw-labs/zeroclaw/" \
  "${IMAGE}:${TAG}"
```

For pinned deployments, resolve the digest and verify `${IMAGE}@${DIGEST}`
instead of a mutable tag.
