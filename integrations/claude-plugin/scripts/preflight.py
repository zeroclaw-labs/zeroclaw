"""Claude Code onboarding MCP; native-onboard owns configuration and readiness."""

import json
import importlib.util
import os
from pathlib import Path
import re
import shutil
import signal
import stat
import subprocess
import sys
import threading
import time

TIMEOUT_SECONDS = 5.0
APPLY_TIMEOUT_SECONDS = 150.0
MAX_CLI_BYTES = 65536
MAX_INPUT_BYTES = 32768
MAX_OUTPUT_BYTES = 16384
PROTOCOL_VERSIONS = {"2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"}
AUTH_STATUS_ARGV = ("auth", "status", "--json")


def load_supervision():
    try:
        path = Path(__file__).resolve().parent / "process_supervision.py"
        spec = importlib.util.spec_from_file_location("zeroclaw_process_supervision", path)
        module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = module
        spec.loader.exec_module(module)
        return module
    except (OSError, ImportError):
        return None


SUPERVISION = load_supervision()


def require(condition, reason):
    if not condition:
        raise ValueError(reason)


def load_package_metadata():
    try:
        path = Path(__file__).resolve().parents[1] / ".claude-plugin" / "plugin.json"
        with path.open("rb") as manifest:
            raw = manifest.read(MAX_INPUT_BYTES + 1)
        require(len(raw) <= MAX_INPUT_BYTES, "invalid_package_metadata")
        parsed = json.loads(raw)
        require(type(parsed) is dict and type(parsed.get("metadata")) is dict, "invalid_package_metadata")
        version = parsed.get("version")
        minimum = parsed["metadata"].get("minimumClaudeCodeVersion")
        require(all(isinstance(value, str) and re.fullmatch(r"\d{1,3}\.\d{1,3}\.\d{1,3}", value)
                    for value in (version, minimum)), "invalid_package_metadata")
        return {"version": version, "minimum": tuple(int(n) for n in minimum.split("."))}
    except (OSError, ValueError, TypeError, RecursionError):
        return None


PACKAGE_METADATA = load_package_metadata()
VERSION = PACKAGE_METADATA["version"] if PACKAGE_METADATA is not None else None
MIN_CLAUDE_VERSION = PACKAGE_METADATA["minimum"] if PACKAGE_METADATA is not None else None


def arguments_object(arguments, allowed):
    require(type(arguments) is dict and not (arguments.keys() - allowed), "invalid_arguments")


def directory_reference(value):
    require(isinstance(value, str) and 0 < len(value) <= 512 and
            not any(ord(c) < 32 or ord(c) == 127 for c in value), "invalid_directory")
    path = Path(value)
    require(path.is_absolute() and ".." not in path.parts, "absolute_directory_required")
    return path


def account_directory(arguments, env):
    value = arguments.get("claude_config_dir", env.get("CLAUDE_CONFIG_DIR"))
    if value is None:
        return {"source": "native_default", "path": None}
    path = directory_reference(value)
    require(path.is_dir(), "account_directory_must_exist")
    return {"source": "selected" if "claude_config_dir" in arguments else "inherited",
            "path": str(path)}


def terminate(process):
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def resolve_cli(name, env):
    # Relative and empty PATH entries must not turn the current project into
    # launcher authority. Explicit absolute installation directories remain
    # operator-selected; this does not authenticate the binary's publisher.
    directories = [entry for entry in env.get("PATH", "").split(os.pathsep)
                   if entry and Path(entry).is_absolute()]
    if not directories:
        return None
    value = shutil.which(name, path=os.pathsep.join(directories))
    if value is None or not Path(value).is_absolute():
        return None
    return str(Path(value).resolve())


def executable_identity(path):
    if path is None:
        return None
    try:
        info = os.stat(path, follow_symlinks=False)
        require(stat.S_ISREG(info.st_mode), "invalid_executable")
        return info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns
    except OSError:
        return None


def reject_nonfinite_json(_value):
    raise ValueError("invalid_json")


def run_cli(executable, argv, env):
    if os.name != "posix":
        return "unsupported_platform", None
    # stderr is discarded at the pipe boundary: it may contain credentials.
    try:
        process = subprocess.Popen([executable, *argv], stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                   env=env, shell=False, start_new_session=True,
                                   bufsize=0)
    except OSError:
        return "unusable_cli", None
    output = bytearray()
    exceeded = threading.Event()

    def read_output():
        while True:
            block = process.stdout.read(4096)
            if not block:
                break
            if len(output) + len(block) > MAX_CLI_BYTES:
                exceeded.set()
                terminate(process)
                break
            output.extend(block)

    reader = threading.Thread(target=read_output, daemon=True)
    reader.start()
    deadline = time.monotonic() + TIMEOUT_SECONDS
    outcome = "ok"
    try:
        process.wait(timeout=TIMEOUT_SECONDS)
        reader.join(max(0, deadline - time.monotonic()))
        if reader.is_alive():
            outcome = "cli_timeout"
    except subprocess.TimeoutExpired:
        outcome = "cli_timeout"
    if exceeded.is_set():
        outcome = "cli_output_limit"
    if outcome != "ok":
        terminate(process)
        try:
            process.wait(timeout=0.2)
        except subprocess.TimeoutExpired:
            pass
        reader.join(0.2)
    if not reader.is_alive():
        process.stdout.close()
    if outcome != "ok":
        return outcome, None
    if process.returncode != 0:
        if process.returncode == 1 and tuple(argv) == AUTH_STATUS_ARGV:
            try:
                parsed = json.loads(output, parse_constant=reject_nonfinite_json)
            except (ValueError, TypeError, RecursionError):
                return "command_failed", None
            if type(parsed) is dict and parsed.get("loggedIn") is False:
                return "ok", b'{"loggedIn":false}'
        return "command_failed", None
    return "ok", bytes(output)


def environment_indicators(env):
    selectors = {
        "bearer_token": ("ANTHROPIC_AUTH_TOKEN",),
        "api_key": ("ANTHROPIC_API_KEY",),
        "headless_code_token": ("CLAUDE_CODE_OAUTH_TOKEN",),
        "console_profile": ("ANTHROPIC_PROFILE",),
        "federation_variables": ("ANTHROPIC_FEDERATION_RULE_ID", "ANTHROPIC_ORGANIZATION_ID"),
        "cloud_provider": ("CLAUDE_CODE_USE_BEDROCK", "CLAUDE_CODE_USE_VERTEX", "CLAUDE_CODE_USE_FOUNDRY"),
        "custom_endpoint": ("ANTHROPIC_BASE_URL",),
    }
    return [label for label, names in selectors.items() if any(env.get(n) for n in names)]


def auth_summary(parsed, env, expected):
    require(type(parsed) is dict and type(parsed.get("loggedIn")) is bool, "malformed_auth_status")
    modes = {"claude.ai": "claude_subscription", "oauth_token": "native_code_token",
             "api_key": "anthropic_api", "apiKeyHelper": "anthropic_api", "api_key_helper": "anthropic_api",
             "anthropic_profile": "console_api", "console": "console_api",
             "bedrock": "cloud_provider", "vertex": "cloud_provider",
             "foundry": "cloud_provider", "gateway": "cloud_gateway", "none": "unauthenticated"}
    method = parsed.get("authMethod")
    method = method if isinstance(method, str) and method in modes else "unknown"
    provider = parsed.get("apiProvider")
    provider = provider if isinstance(provider, str) and provider in {
        "firstParty", "bedrock", "vertex", "foundry", "gateway", "customEndpoint"} else "unknown"
    mode = modes.get(method, "unknown")
    if method == "anthropic_profile":
        profile_mode = parsed.get("profileAuthMode")
        mode = {"user_oauth": "console_api", "oidc_federation": "federation_api"}.get(profile_mode, "unknown")
    expected_providers = {
        "claude_subscription": {"firstParty"}, "native_code_token": {"firstParty"},
        "anthropic_api": {"firstParty"}, "console_api": {"firstParty"}, "federation_api": {"firstParty"},
        "cloud_provider": {"bedrock", "vertex", "foundry"}, "cloud_gateway": {"gateway"},
    }
    if mode in expected_providers and provider not in expected_providers[mode]:
        mode = "ambiguous"
    indicators = environment_indicators(env)
    # auth status is a report, not an inference probe. Conflicting selectors or
    # a custom endpoint need the operator's native /status review; never change them.
    permitted_native_selectors = {"headless_code_token"} if mode == "native_code_token" else set()
    if mode in {"claude_subscription", "native_code_token"} and set(indicators) - permitted_native_selectors:
        mode = "ambiguous"
    conflicting_selectors = {
        "anthropic_api": {"cloud_provider", "custom_endpoint", "bearer_token"},
        "console_api": {"cloud_provider", "custom_endpoint", "bearer_token", "api_key", "headless_code_token"},
        "federation_api": {"cloud_provider", "custom_endpoint", "bearer_token", "api_key", "headless_code_token"},
    }
    if set(indicators) & conflicting_selectors.get(mode, set()):
        mode = "ambiguous"
    if not parsed["loggedIn"]:
        mode = "unauthenticated"
    billing = {"claude_subscription": "claude_account_plan", "native_code_token": "claude_account_plan",
               "anthropic_api": "anthropic_api", "console_api": "anthropic_api",
               "federation_api": "anthropic_api", "cloud_provider": "cloud_provider_account",
               "cloud_gateway": "gateway_account", "unauthenticated": "none"}.get(mode, "unknown")
    match = None if billing == "unknown" else billing in {
        "subscription": {"claude_account_plan"}, "api": {"anthropic_api"},
        "cloud_or_gateway": {"cloud_provider_account", "gateway_account"}}[expected]
    # Only constructed enums and a validated boolean leave this boundary.
    return {"logged_in": parsed["loggedIn"], "reported_method": method,
            "reported_provider": provider, "credential_mode": mode, "billing_source": billing,
            "expected_billing": expected, "billing_matches_expectation": match,
            "billing_status": "unknown" if match is None else "matches" if match else "mismatch",
            "environment_indicators": indicators, "inference_verified": False}


def status(arguments, executables=None):
    require(PACKAGE_METADATA is not None, "invalid_package_metadata")
    arguments_object(arguments, {"claude_config_dir", "expected_billing"})
    expected = arguments.get("expected_billing", "subscription")
    require(expected in ("subscription", "api", "cloud_or_gateway"), "invalid_expected_billing")
    native = {"cli_status": "unsupported_platform", "version": None,
              "account_directory": {"source": "not_checked", "path": None}, "auth": None}
    result = {"native_code": native,
              "zeroclaw_engine": {"status": "requires_configuration", "native_code_backend": "claude_code_native",
                                  "bootstrap_cli_status": "not_checked",
                                  "claude_code_alias": "direct_anthropic_http", "inference_verified": False}}
    if os.name != "posix":
        return result
    env = dict(os.environ)
    directory = account_directory(arguments, env)
    if directory["path"] is not None:
        env["CLAUDE_CONFIG_DIR"] = directory["path"]
    native.update(cli_status="missing_cli", account_directory=directory)
    executables = executables if executables is not None else {
        "claude": resolve_cli("claude", env), "zeroclaw": resolve_cli("zeroclaw", env)}
    executable = executables["claude"]
    if executable is None:
        return result
    outcome, output = run_cli(executable, ["--version"], env)
    if outcome != "ok":
        native["cli_status"] = "unusable_cli" if outcome == "command_failed" else outcome
        return result
    version = re.fullmatch(rb"(\d{1,3})\.(\d{1,3})\.(\d{1,3}) \(Claude Code\)", output.strip())
    if version is None:
        native["cli_status"] = "unrecognized_cli"
        return result
    numbers = tuple(int(n) for n in version.groups())
    native["version"] = ".".join(str(n) for n in numbers)
    if numbers < MIN_CLAUDE_VERSION:
        native["cli_status"] = "unsupported_cli_version"
        return result
    outcome, output = run_cli(executable, AUTH_STATUS_ARGV, env)
    if outcome != "ok":
        native["cli_status"] = "auth_status_failed" if outcome == "command_failed" else outcome
        return result
    try:
        native["auth"] = auth_summary(json.loads(output), env, expected)
    except (ValueError, TypeError, RecursionError):
        native["cli_status"] = "malformed_auth_status"
        return result
    native["cli_status"] = "available"
    zeroclaw = executables["zeroclaw"]
    engine = result["zeroclaw_engine"]
    engine["bootstrap_cli_status"] = "missing_zeroclaw"
    if zeroclaw is not None:
        outcome, help_output = run_cli(zeroclaw, ["native-onboard", "--help"], env)
        engine["bootstrap_cli_status"] = "incompatible_zeroclaw"
        if outcome == "ok" and all(flag in help_output for flag in (
                b"claude-code", b"--client", b"--provider-alias", b"--agent-alias", b"--model",
                b"--risk-preset", b"--expected-billing", b"--accept-yolo", b"--accept-api-billing",
                b"--native-config-dir")):
            engine["bootstrap_cli_status"] = "available"
    return result


def plan(arguments, allow_resume=False):
    require(PACKAGE_METADATA is not None, "invalid_package_metadata")
    arguments_object(arguments, {"instance_root", "provider_alias", "agent_alias", "risk_preset",
                                 "accept_yolo", "engine_backend", "accept_api_billing", "claude_config_dir",
                                 "expected_billing", "model"})
    root = directory_reference(arguments.get("instance_root"))
    require(allow_resume or not os.path.lexists(root), "fresh_instance_root_required")
    require(root.parent.is_dir() and root.parent.resolve() == root.parent, "canonical_existing_parent_required")
    account = account_directory(arguments, os.environ)
    account_path = account["path"] or str(Path(os.environ.get("HOME", str(Path.home()))) / ".claude")
    account_path = Path(account_path).resolve()
    require(root != account_path and account_path not in root.parents and root not in account_path.parents,
            "account_instance_overlap")
    provider_alias = arguments.get("provider_alias")
    agent_alias = arguments.get("agent_alias")
    require(all(isinstance(v, str) and re.fullmatch(r"[a-z][a-z0-9_]{0,47}", v)
                for v in (provider_alias, agent_alias)), "invalid_alias")
    risk = arguments.get("risk_preset", "balanced")
    accept_yolo = arguments.get("accept_yolo", False)
    require(risk in ("balanced", "yolo") and type(accept_yolo) is bool and
            accept_yolo == (risk == "yolo"), "explicit_risk_choice_required")
    backend = arguments.get("engine_backend", "native_claude_code")
    expected = arguments.get("expected_billing", "subscription")
    require(expected in ("subscription", "api", "cloud_or_gateway"), "invalid_expected_billing")
    model = arguments.get("model", "default")
    require(isinstance(model, str) and re.fullmatch(r"[A-Za-z][A-Za-z0-9_.:/\[\]-]{0,127}", model), "invalid_model")
    accept_api = arguments.get("accept_api_billing", False)
    require(backend in ("native_claude_code", "anthropic_api") and type(accept_api) is bool and
            accept_api == (backend == "anthropic_api" or expected == "api"), "explicit_engine_billing_choice_required")
    provider_ref = ("anthropic." if backend == "anthropic_api" else "claude_code_native.") + provider_alias
    native_argv = ["zeroclaw", "--config-dir", str(root), "native-onboard", "--client", "claude-code",
                   "--provider-alias", provider_alias, "--agent-alias", agent_alias,
                   "--model", model, "--risk-preset", risk, "--expected-billing", expected]
    if accept_yolo:
        native_argv.append("--accept-yolo")
    if accept_api:
        native_argv.append("--accept-api-billing")
    if account["path"] is not None:
        native_argv.extend(["--native-config-dir", account["path"]])
    return {"status": "requires_configuration",
            "instance_root": str(root), "writes_performed": False,
            "native_account_directory": account,
            "provider": {"alias": provider_alias, "engine_backend": backend,
                         "config_reference": provider_ref, "authentication_status": "requires_configuration",
                         "model": model, "expected_billing": "api" if backend == "anthropic_api" else expected,
                         "billing_source": "anthropic_api" if backend == "anthropic_api" else "native_client_account"},
            "agent": {"alias": agent_alias, "model_provider": provider_ref, "risk_profile": risk},
            "risk": {"preset": risk, "canonical_source": "zeroclaw_config::presets::RISK_PRESETS",
                     "effective_policy_status": "unresolved", "accept_yolo": accept_yolo},
            "inference_verified": False,
            "terminal_handoff": {"human_terminal_required": True, "owner": "zeroclaw quickstart",
                                 "argv": ["zeroclaw", "--config-dir", str(root), "quickstart",
                                          "--model-provider", "anthropic", "--agent", agent_alias],
                                 "provider_alias_selection": provider_alias, "risk_preset_selection": risk,
                                 "recheck_fresh_root_before_execution": True} if backend == "anthropic_api" else {
                                     "human_terminal_required": True, "owner": "zeroclaw native-onboard",
                                     "argv": native_argv, "requires_bootstrap_cli_status": "available",
                                     "recheck_fresh_root_before_execution": True}}


def hold_prior_receipt(root):
    """Keep the previous publication inode allocated until this call finishes."""
    directory_fd = receipt_fd = None
    try:
        directory_fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        info = os.fstat(directory_fd)
        require(info.st_uid == os.geteuid() and info.st_mode & 0o077 == 0, "invalid_receipt")
        receipt_fd = os.open("native-onboard.json", os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK,
                             dir_fd=directory_fd)
        info = os.fstat(receipt_fd)
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.geteuid() and
                info.st_mode & 0o077 == 0 and info.st_size < MAX_CLI_BYTES, "invalid_receipt")
        result = receipt_fd, (info.st_dev, info.st_ino)
        receipt_fd = None
        return result
    except FileNotFoundError:
        return None, None
    finally:
        if receipt_fd is not None:
            os.close(receipt_fd)
        if directory_fd is not None:
            os.close(directory_fd)


def receipt_observation(root, expected_request, earliest, previous_publication=None):
    """Read only the CLI's private receipt, reconstructing an allowlisted result."""
    root_fd = receipt_fd = None
    try:
        root_fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        root_info = os.fstat(root_fd)
        require(root_info.st_uid == os.geteuid() and root_info.st_mode & 0o077 == 0,
                "invalid_receipt")
        receipt_fd = os.open("native-onboard.json", os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK,
                             dir_fd=root_fd)
        info = os.fstat(receipt_fd)
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.geteuid() and
                info.st_mode & 0o077 == 0 and info.st_size < MAX_CLI_BYTES, "invalid_receipt")
        require(previous_publication != (info.st_dev, info.st_ino), "receipt_not_republished")
        with os.fdopen(receipt_fd, "rb") as receipt:
            receipt_fd = None
            raw = receipt.read(MAX_CLI_BYTES + 1)
        require(len(raw) <= MAX_CLI_BYTES, "invalid_receipt")
        parsed = json.loads(raw, parse_constant=reject_nonfinite_json)
        require(type(parsed) is dict and type(parsed.get("schema_version")) is int and parsed["schema_version"] == 1 and
                parsed.get("root_identity") == [root_info.st_dev, root_info.st_ino] and
                json.dumps(parsed.get("request"), sort_keys=True) == json.dumps(expected_request, sort_keys=True),
                "invalid_receipt")
        phase = parsed.get("phase")
        require(phase in {"pending_auth", "configured", "ready", "failed"}, "invalid_receipt")
        observation = parsed.get("last_validation")
        timestamp = None
        if phase == "ready":
            require(type(observation) is dict and
                    type(observation.get("at_unix_seconds")) is int and
                    earliest <= observation["at_unix_seconds"] <= int(time.time()) + 1 and
                    observation.get("model_provider") == "claude_code_native." + expected_request["provider_alias"] and
                    observation.get("model") == expected_request["model"], "invalid_receipt")
            timestamp = observation["at_unix_seconds"]
        return phase, timestamp
    except (OSError, ValueError, TypeError, RecursionError):
        return "invalid_receipt", None
    finally:
        if receipt_fd is not None:
            os.close(receipt_fd)
        if root_fd is not None:
            os.close(root_fd)


def interrupt_apply(process):
    return SUPERVISION.interrupt_and_reap(process)


def apply_instance(arguments, cancellation=None):
    arguments_object(arguments, {"instance_root", "provider_alias", "agent_alias", "risk_preset",
                                 "accept_yolo", "engine_backend", "accept_api_billing", "claude_config_dir",
                                 "expected_billing", "model", "confirm_create", "resume"})
    require(arguments.get("confirm_create") is True, "explicit_creation_required")
    resume = arguments.get("resume", False)
    require(type(resume) is bool, "explicit_resume_required")
    require(arguments.get("engine_backend", "native_claude_code") == "native_claude_code",
            "native_backend_required")
    planned = plan({key: value for key, value in arguments.items() if key not in {"confirm_create", "resume"}},
                   allow_resume=resume)
    result = {"status": "unsupported_platform", "instance_root": planned["instance_root"],
              "configuration_owner": "zeroclaw native-onboard", "inference_verified": False,
              "provider_reference": planned["provider"]["config_reference"], "last_validation_at": None}
    if os.name != "posix":
        return result
    if SUPERVISION is None or not SUPERVISION.check_support():
        result["status"] = "process_supervision_unavailable"
        return result
    expected = planned["provider"]["expected_billing"]
    env = dict(os.environ)
    executables = {"claude": resolve_cli("claude", env), "zeroclaw": resolve_cli("zeroclaw", env)}
    identities = {name: executable_identity(path) for name, path in executables.items()}
    native = status({key: value for key, value in arguments.items() if key in {"claude_config_dir", "expected_billing"}},
                    executables=executables)
    engine_status = native["zeroclaw_engine"]["bootstrap_cli_status"]
    if engine_status != "available":
        result["status"] = engine_status
        return result
    auth = native["native_code"]["auth"]
    if auth is None or auth.get("billing_matches_expectation") is not True or auth.get("logged_in") is not True:
        result["status"] = "native_billing_not_verified"
        return result
    executable = executables["zeroclaw"]
    require(executable is not None, "missing_zeroclaw")
    require(all(identity is not None and executable_identity(executables[name]) == identity
                for name, identity in identities.items()), "admitted_executable_changed")
    argv = planned["terminal_handoff"]["argv"][1:]
    # Native default and an explicit override may have different credential
    # namespaces even when their filesystem locations look identical. Preserve
    # the chosen selector; the provider pins None to native default at use time.
    selected = argv[argv.index("--native-config-dir") + 1] if "--native-config-dir" in argv else None
    expected_request = {"client": "claude-code", "provider_alias": arguments["provider_alias"],
                        "agent_alias": arguments["agent_alias"], "model": planned["provider"]["model"],
                        "risk_preset": planned["risk"]["preset"], "expected_billing": expected,
                        "accept_yolo": planned["risk"]["accept_yolo"],
                        "accept_api_billing": arguments.get("accept_api_billing", False),
                        "native_config_dir": selected, "auth_profile": "subscriber"}
    cancellation = cancellation or threading.Event()
    if cancellation.is_set():
        result["status"] = "apply_cancelled"
        return result
    earliest = int(time.time())
    prior_fd, previous_publication = hold_prior_receipt(Path(planned["instance_root"]))
    try:
        # Neither raw CLI stream can reach chat, memory or a diagnostic file.
        process = subprocess.Popen([executable, *argv], stdin=subprocess.DEVNULL,
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                   env=env, shell=False, start_new_session=True)
    except OSError:
        if prior_fd is not None:
            os.close(prior_fd)
        result["status"] = "unusable_zeroclaw"
        return result
    deadline = time.monotonic() + APPLY_TIMEOUT_SECONDS
    try:
        while not SUPERVISION.passive_exited(process):
            if cancellation.wait(0.05) or time.monotonic() >= deadline:
                result["status"] = "apply_cancelled" if cancellation.is_set() else "apply_timeout"
                interrupt_apply(process)
                return result
        SUPERVISION.cleanup_and_reap(process)
        phase, timestamp = receipt_observation(Path(planned["instance_root"]), expected_request, earliest,
                                              previous_publication)
        if process.returncode != 0:
            result["status"] = "apply_failed"
            return result
        result.update(status=phase, inference_verified=phase == "ready", last_validation_at=timestamp)
        return result
    except SUPERVISION.SupervisionError:
        result["status"] = "cleanup_unverified"
        return result
    finally:
        if prior_fd is not None:
            os.close(prior_fd)
        if process.returncode is None:
            try:
                SUPERVISION.cleanup_and_reap(process)
            except SUPERVISION.SupervisionError:
                result.update(status="cleanup_unverified", inference_verified=False,
                              last_validation_at=None)


def object_schema(properties, required=()):
    return {"type": "object", "properties": properties, "required": list(required), "additionalProperties": False}


PATH_SCHEMA = {"type": "string", "maxLength": 512, "description": "Operator-selected absolute directory reference; never credentials."}
ALIAS_SCHEMA = {"type": "string", "pattern": "^[a-z][a-z0-9_]{0,47}$"}
TOOLS = [
    {"name": "bootstrap.status", "description": "Read native Claude Code version/auth status; no login, inference or configuration writes.",
     "inputSchema": object_schema({"claude_config_dir": PATH_SCHEMA,
                                   "expected_billing": {"type": "string", "enum": ["subscription", "api", "cloud_or_gateway"]}})},
    {"name": "bootstrap.plan", "description": "Preview named native-provider/agent/risk references and a canonical fresh-instance terminal handoff; no writes or inference.",
     "inputSchema": object_schema({"instance_root": PATH_SCHEMA, "provider_alias": ALIAS_SCHEMA, "agent_alias": ALIAS_SCHEMA,
                                   "claude_config_dir": PATH_SCHEMA,
                                   "model": {"type": "string", "maxLength": 128, "default": "default"},
                                   "expected_billing": {"type": "string", "enum": ["subscription", "api", "cloud_or_gateway"]},
                                   "risk_preset": {"type": "string", "enum": ["balanced", "yolo"], "default": "balanced"},
                                   "accept_yolo": {"type": "boolean", "default": False},
                                   "engine_backend": {"type": "string", "enum": ["native_claude_code", "anthropic_api"]},
                                   "accept_api_billing": {"type": "boolean", "default": False}},
                                  ("instance_root", "provider_alias", "agent_alias"))},
]
for tool in TOOLS:
    tool["annotations"] = {"readOnlyHint": True, "destructiveHint": False, "idempotentHint": True, "openWorldHint": False}

TOOLS.append({"name": "bootstrap.apply", "description": "Create or explicitly resume the previewed native instance through canonical native-onboard; may consume selected model usage. Requires explicit creation and risk/billing choices.",
              "inputSchema": object_schema({**TOOLS[1]["inputSchema"]["properties"],
                                            "engine_backend": {"type": "string", "enum": ["native_claude_code"]},
                                            "confirm_create": {"type": "boolean", "const": True},
                                            "resume": {"type": "boolean", "default": False}},
                                           ("instance_root", "provider_alias", "agent_alias", "confirm_create")),
              "annotations": {"readOnlyHint": False, "destructiveHint": False,
                              "idempotentHint": False, "openWorldHint": True}})


def error(request_id, code, message):
    return {"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": message}}


def handle(request, cancellation=None):
    if type(request) is not dict:
        return error(None, -32600, "invalid_request")
    request_id = request.get("id")
    valid_id = request_id is None or type(request_id) is int or (
        isinstance(request_id, str) and len(request_id) <= 64 and request_id.isascii())
    if request.get("jsonrpc") != "2.0" or not valid_id or not isinstance(request.get("method"), str):
        return error(None, -32600, "invalid_request")
    if PACKAGE_METADATA is None:
        return error(request_id, -32603, "invalid_package_metadata")
    method = request["method"]
    if "id" not in request:
        return None
    params = request.get("params", {})
    if type(params) is not dict:
        return error(request_id, -32602, "invalid_params")
    if method == "initialize":
        requested = params.get("protocolVersion")
        result = {"protocolVersion": requested if isinstance(requested, str) and requested in PROTOCOL_VERSIONS else "2024-11-05",
                  "capabilities": {"tools": {"listChanged": False}},
                  "serverInfo": {"name": "zeroclaw-onboarding-preflight", "version": VERSION}}
    elif method == "ping":
        result = {}
    elif method == "tools/list":
        result = {"tools": TOOLS}
    elif method == "tools/call":
        name = params.get("name")
        arguments = params.get("arguments", {})
        try:
            if name == "bootstrap.status":
                value = status(arguments)
            elif name == "bootstrap.plan":
                value = plan(arguments)
            elif name == "bootstrap.apply":
                value = apply_instance(arguments, cancellation)
            else:
                return error(request_id, -32602, "unknown_tool")
            result = {"content": [{"type": "text", "text": json.dumps(value, separators=(",", ":"))}],
                      "isError": name == "bootstrap.apply" and value["status"] != "ready"}
        except ValueError:
            result = {"content": [{"type": "text", "text": '{"status":"invalid_input"}'}], "isError": True}
    else:
        return error(request_id, -32601, "unknown_method")
    return {"jsonrpc": "2.0", "id": request_id, "result": result}


def stdio():
    write_lock = threading.Lock()
    active = {}

    def send(response):
        if response is not None:
            encoded = json.dumps(response, separators=(",", ":")).encode("utf-8")
            if len(encoded) > MAX_OUTPUT_BYTES:
                encoded = json.dumps(error(None, -32603, "output_limit")).encode("utf-8")
            with write_lock:
                try:
                    sys.stdout.buffer.write(encoded + b"\n")
                    sys.stdout.buffer.flush()
                except BrokenPipeError:
                    for worker, cancellation in active.values():
                        cancellation.set()

    def execute(request, cancellation):
        try:
            send(handle(request, cancellation))
        except (OSError, TypeError):
            send(error(request.get("id"), -32603, "internal_error"))

    try:
        stdio_read(send, execute, active)
    except KeyboardInterrupt:
        return
    finally:
        for worker, cancellation in active.values():
            cancellation.set()
        for worker, cancellation in active.values():
            worker.join(3.0)


def stdio_read(send, execute, active):
    while True:
        line = sys.stdin.buffer.readline(MAX_INPUT_BYTES + 1)
        if not line:
            return
        if len(line) > MAX_INPUT_BYTES:
            response = error(None, -32600, "input_limit")
        else:
            try:
                request = json.loads(line)
                if type(request) is dict and request.get("jsonrpc") == "2.0" and request.get("method") == "notifications/cancelled" and "id" not in request:
                    params = request.get("params")
                    request_id = params.get("requestId") if type(params) is dict else None
                    if type(request_id) in {int, str} and request_id in active:
                        active[request_id][1].set()
                    continue
                if type(request) is dict and request.get("method") == "tools/call" and type(request.get("params")) is dict and request["params"].get("name") == "bootstrap.apply" and type(request.get("id")) in {int, str}:
                    for key in list(active):
                        if not active[key][0].is_alive():
                            del active[key]
                    if active:
                        response = error(request.get("id"), -32603, "apply_already_running")
                    else:
                        cancellation = threading.Event()
                        worker = threading.Thread(target=execute, args=(request, cancellation))
                        active[request["id"]] = (worker, cancellation)
                        worker.start()
                        continue
                else:
                    response = handle(request)
            except (ValueError, UnicodeError, RecursionError):
                response = error(None, -32700, "invalid_json")
            except (OSError, TypeError):
                response = error(None, -32603, "internal_error")
        send(response)
        if len(line) > MAX_INPUT_BYTES:
            return


if __name__ == "__main__":
    if sys.argv[1:] == ["--stdio"]:
        def shutdown(_signum, _frame):
            raise KeyboardInterrupt
        signal.signal(signal.SIGTERM, shutdown)
        stdio()
    else:
        sys.exit(2)
