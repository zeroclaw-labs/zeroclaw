#!/usr/bin/env bash
set -euo pipefail

# Focused security regression suite covering critical auth/policy/secret paths.
# Keep tests narrowly scoped and deterministic so they can run in security CI.
TESTS=(
  run_tool_call_loop_denies_supervised_tools_on_non_cli_channels
  run_tool_call_loop_blocks_tools_excluded_for_channel
  webhook_rejects_public_traffic_without_auth_layers
  metrics_endpoint_rejects_public_clients_when_pairing_is_disabled
  metrics_endpoint_requires_bearer_token_when_pairing_is_enabled
  extract_ws_bearer_token_rejects_empty_tokens
  autonomy_config_serde_defaults_non_cli_excluded_tools
  config_validate_rejects_duplicate_non_cli_excluded_tools
  config_debug_redacts_sensitive_values
  config_save_encrypts_nested_credentials
  replayed_totp_code_is_rejected
  validate_command_execution_rejects_forbidden_paths
  screenshot_path_validation_blocks_escaped_paths
  test_execute_blocked_in_read_only_mode
  key_file_created_on_first_encrypt
  scrub_google_api_key_prefix
  scrub_aws_access_key_prefix
)

CARGO_BIN="${CARGO_BIN:-cargo}"

for test_name in "${TESTS[@]}"; do
  echo "==> ${CARGO_BIN} test --locked --lib ${test_name}"
  "${CARGO_BIN}" test --locked --lib "${test_name}" -- --nocapture
done
