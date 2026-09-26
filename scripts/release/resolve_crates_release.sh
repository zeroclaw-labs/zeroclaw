#!/usr/bin/env bash
set -euo pipefail

# resolve_crates_release.sh: pin the commit a crates.io publisher run acts on.
#
# Run from the checked-out release tree. Inputs come from the environment:
#
#   RELEASE_TAG               vX.Y.Z (required)
#   RELEASE_SHA               commit the release was built from
#   STAGE                     all (default) | preflight | publish
#   VERIFIED_WEB_DIST_DIGEST  stage publish only: digest an earlier preflight
#                             stage recorded for web/dist
#
# Prints stage=, version=, sha= and msrv= lines for $GITHUB_OUTPUT.
#
# Stages exist so the stable release can verify its crates before anything
# irreversible happens. The `preflight` stage runs beside the binary builds,
# before the GitHub Release creates the tag, so it may verify RELEASE_SHA with
# no tag yet. The `publish` stage runs after the GitHub Release and requires
# the tag to exist and resolve to that same commit. `all` is the standalone
# path: preflight and publish in one run against an existing tag.

stage="${STAGE:-all}"
case "$stage" in
  all | preflight | publish) ;;
  *)
    echo "::error::stage must be all, preflight, or publish. Got: ${stage}" >&2
    exit 1
    ;;
esac

if [[ ! "${RELEASE_TAG:-}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "::error::release_tag must be vX.Y.Z format. Got: ${RELEASE_TAG:-}" >&2
  exit 1
fi

version="${RELEASE_TAG#v}"
cargo_version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)"
msrv="$(sed -n 's/^rust-version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)"

if [[ "$cargo_version" != "$version" ]]; then
  echo "::error::Tag ${RELEASE_TAG} does not match workspace version ${cargo_version}." >&2
  exit 1
fi
if [[ ! "$msrv" =~ ^[0-9]+\.[0-9]+(\.[0-9]+)?$ ]]; then
  echo "::error::Could not read [workspace.package] rust-version (got '${msrv}')." >&2
  exit 1
fi

# The staged path splits one release across two calls in the same run. Both
# calls must name the commit the binaries were built from, or the second call
# could publish a tree the first never verified.
if [[ "$stage" != "all" && -z "${RELEASE_SHA:-}" ]]; then
  echo "::error::stage ${stage} requires release_sha." >&2
  exit 1
fi
if [[ "$stage" == "publish" ]]; then
  if [[ ! "${VERIFIED_WEB_DIST_DIGEST:-}" =~ ^[0-9a-f]{64}$ ]]; then
    echo "::error::stage publish requires the web/dist digest recorded by the earlier preflight stage." >&2
    exit 1
  fi
elif [[ -n "${VERIFIED_WEB_DIST_DIGEST:-}" ]]; then
  echo "::error::verified_web_dist_digest is only valid with stage publish." >&2
  exit 1
fi

head="$(git rev-parse HEAD)"

# The format check above only proves the string looks like a tag. A branch of
# the same name would resolve too, so read it from refs/tags explicitly.
if git rev-parse -q --verify "refs/tags/${RELEASE_TAG}^{commit}" >/dev/null; then
  sha="$(git rev-parse "refs/tags/${RELEASE_TAG}^{commit}")"
elif [[ "$stage" == "preflight" ]]; then
  # Before the GitHub Release creates the tag, verify the commit it will be
  # created at. The publish stage refuses to upload unless the tag then
  # resolves to this same commit.
  sha="$head"
else
  echo "::error::${RELEASE_TAG} is not a tag in this repository." >&2
  exit 1
fi

# When the caller knows which commit the release was built from, refuse to
# act on anything else. Without this, the tag could be moved between building
# the binaries and publishing the crates, and the two would describe
# different trees under one version number.
if [[ -n "${RELEASE_SHA:-}" && "$sha" != "$RELEASE_SHA" ]]; then
  echo "::error::Tag ${RELEASE_TAG} resolves to ${sha}, but the release was built from ${RELEASE_SHA}." >&2
  exit 1
fi
# Everything above read Cargo.toml from the working tree, so it must be the
# pinned commit and not merely a tree with the same version string.
if [[ "$head" != "$sha" ]]; then
  echo "::error::The checkout is ${head}, but ${RELEASE_TAG} is pinned to ${sha}." >&2
  exit 1
fi

echo "stage=$stage"
echo "version=$version"
echo "sha=$sha"
echo "msrv=$msrv"
