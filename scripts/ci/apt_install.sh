#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -eq 0 ]]; then
  echo "usage: $0 <package> [package ...]" >&2
  exit 2
fi

# GitHub-hosted Ubuntu images sometimes ship transient Microsoft apt sources.
# They are unrelated to this workflow and can break apt-get update before CI
# installs its own packages.
apt_sources=(
  /etc/apt/sources.list.d/azure-cli.*
  /etc/apt/sources.list.d/microsoft-prod.*
)

for apt_source in "${apt_sources[@]}"; do
  if [[ -e "$apt_source" || -L "$apt_source" ]]; then
    echo "Removing runner-provided apt source: $apt_source"
    sudo rm -f "$apt_source"
  fi
done

sudo apt-get update -qq
sudo apt-get install -y "$@"
