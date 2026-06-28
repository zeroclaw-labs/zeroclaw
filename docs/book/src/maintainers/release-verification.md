# Release Artifact Verification

ZeroClaw signs all release artifacts using [Sigstore](https://sigstore.dev) keyless signing.
Signatures are recorded in the [Rekor](https://rekor.sigstore.dev) public transparency log, so no
private key material is stored anywhere.

## Prerequisites

```bash
# Install cosign (https://docs.sigstore.dev/cosign/system_config/installation/)
go install github.com/sigstore/cosign/v2/cmd/cosign@latest
# or via Homebrew
brew install cosign
```

---

## Binary Release Assets

Every `.tar.gz`, `.zip`, and `.dmg` in a ZeroClaw GitHub Release is accompanied by a
`.bundle` file containing the certificate chain and signature.

### Verify a binary release asset

```bash
# Replace VERSION and TARGET with the appropriate values, e.g.:
#   VERSION=v1.5.0
#   TARGET=x86_64-unknown-linux-gnu

VERSION=v1.5.0
TARGET=x86_64-unknown-linux-gnu
ASSET="zeroclaw-${TARGET}.tar.gz"

# Download the asset and its bundle
gh release download "$VERSION" --repo zeroclaw-labs/zeroclaw \
  --pattern "${ASSET}" \
  --pattern "${ASSET}.bundle"

# Verify (cosign checks the Rekor log automatically)
cosign verify-blob \
  --bundle "${ASSET}.bundle" \
  "${ASSET}"
```

A successful verification prints:

```
Verified OK
```

### Expected certificate identity

The signing certificate is issued via GitHub Actions OIDC:

- **OIDC issuer:** `https://token.actions.githubusercontent.com`
- **Subject:** matches `https://github.com/zeroclaw-labs/zeroclaw/.github/workflows/release-stable-manual.yml@refs/tags/vX.Y.Z`

To verify explicitly:

```bash
cosign verify-blob \
  --bundle "${ASSET}.bundle" \
  --certificate-oidc-issuer "https://token.actions.githubusercontent.com" \
  --certificate-identity-regexp "^https://github.com/zeroclaw-labs/zeroclaw/" \
  "${ASSET}"
```

---

## Container Images

ZeroClaw container images on GHCR are signed by digest using the same keyless model.

### Verify a container image

```bash
IMAGE="ghcr.io/zeroclaw-labs/zeroclaw"
TAG="v1.5.0"

cosign verify \
  --certificate-oidc-issuer "https://token.actions.githubusercontent.com" \
  --certificate-identity-regexp "^https://github.com/zeroclaw-labs/zeroclaw/" \
  "${IMAGE}:${TAG}"
```

### Verify by digest (recommended for air-gapped or pinned deployments)

```bash
# Resolve the digest first
DIGEST=$(docker buildx imagetools inspect "${IMAGE}:${TAG}" \
  --format '{{json .Manifest}}' | jq -r '.digest')

cosign verify \
  --certificate-oidc-issuer "https://token.actions.githubusercontent.com" \
  --certificate-identity-regexp "^https://github.com/zeroclaw-labs/zeroclaw/" \
  "${IMAGE}@${DIGEST}"
```

---

## SBOM

Two SBOM files are published alongside each release:

| File | Format |
|------|--------|
| `zeroclaw-vX.Y.Z-sbom.spdx.json` | SPDX 2.3 (JSON) |
| `zeroclaw-vX.Y.Z-sbom.cdx.json` | CycloneDX 1.5 (JSON) |

### Download and inspect

```bash
VERSION=v1.5.0

gh release download "$VERSION" --repo zeroclaw-labs/zeroclaw \
  --pattern "*-sbom.spdx.json" \
  --pattern "*-sbom.cdx.json"

# Inspect with syft (https://github.com/anchore/syft)
syft convert "zeroclaw-${VERSION}-sbom.spdx.json" -o table

# Or with grype for vulnerability scanning
grype "zeroclaw-${VERSION}-sbom.spdx.json"
```

---

## SLSA Provenance

Starting from the version where SLSA provenance generation became blocking, a
`.intoto.jsonl` provenance file is published alongside each release. Earlier releases
may have provenance files generated non-blocking (present but not required for the build
to succeed).

```bash
VERSION=v1.5.0

gh release download "$VERSION" --repo zeroclaw-labs/zeroclaw \
  --pattern "*.intoto.jsonl"

# Verify with slsa-verifier (https://github.com/slsa-framework/slsa-verifier)
slsa-verifier verify-artifact \
  --provenance-path "multiple.intoto.jsonl" \
  --source-uri "github.com/zeroclaw-labs/zeroclaw" \
  --source-tag "$VERSION" \
  zeroclaw-x86_64-unknown-linux-gnu.tar.gz
```

---

## Transparency Log Lookup

All signatures are permanently recorded in Rekor. To look up a signature:

```bash
# By artifact hash
HASH=$(sha256sum zeroclaw-x86_64-unknown-linux-gnu.tar.gz | awk '{print $1}')
rekor-cli search --sha "sha256:${HASH}" --rekor_server https://rekor.sigstore.dev
```

Or browse the Rekor log at: <https://search.sigstore.dev/>
