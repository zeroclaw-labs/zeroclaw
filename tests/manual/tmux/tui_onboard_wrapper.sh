#!/usr/bin/env bash

set -uo pipefail

config_dir="$1"
bin_path="$2"

env ZEROCLAW_CONFIG_DIR="$config_dir" "$bin_path" onboard --tui
status=$?
printf '\nEXIT_STATUS=%s\n' "$status"
sleep 5
