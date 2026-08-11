#!/usr/bin/env bash
set -euo pipefail

dry_run="${DRY_RUN-false}"
credential_canary="${CREDENTIAL_CANARY-false}"
bucket_repo="${SCOOP_BUCKET_REPO:-}"
bucket_token="${GH_TOKEN:-}"

case "$dry_run" in
  true | false) ;;
  *)
    echo "::error::DRY_RUN must be true or false." >&2
    exit 1
    ;;
esac

case "$credential_canary" in
  true | false) ;;
  *)
    echo "::error::CREDENTIAL_CANARY must be true or false." >&2
    exit 1
    ;;
esac

if [[ "$credential_canary" == "true" ]]; then
  if [[ -z "$bucket_repo" ]]; then
    echo "::error::Repository variable SCOOP_BUCKET_REPO is required for the credential canary." >&2
    exit 1
  fi
  if [[ -z "$bucket_token" ]]; then
    echo "::error::Secret SCOOP_BUCKET_TOKEN is required for the credential canary." >&2
    exit 1
  fi
elif [[ -z "$bucket_repo" || -z "$bucket_token" ]]; then
  if [[ "$dry_run" != "true" ]]; then
    echo "::error::Repository variable SCOOP_BUCKET_REPO and secret SCOOP_BUCKET_TOKEN are both required when dry_run=false." >&2
    exit 1
  fi

  echo "::warning::Scoop bucket credentials are unavailable; skipping the push-access probe." >&2
  printf 'skip\n'
  exit 0
fi

if [[ "$bucket_repo" != */* ]]; then
  echo "::error::SCOOP_BUCKET_REPO must be in owner/repo format." >&2
  exit 1
fi

printf 'probe\n'
