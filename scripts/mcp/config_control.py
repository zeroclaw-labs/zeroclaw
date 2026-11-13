#!/usr/bin/env python3
"""Opt-in stdio MCP client for the existing local ZeroClaw configuration API."""

import argparse
import http.client
import ipaddress
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

MAX_MESSAGE = 1_048_576
MAX_RESPONSE = 8_388_608
COMPLETED_JOB_LIMIT = 100
COMPLETED_JOB_MAX_AGE = 7 * 24 * 3600
SUPPORTED_VERSIONS = ("2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25")


class BridgeError(Exception):
    def __init__(self, error_key, **details):
        super().__init__(error_key)
        self.details = {"error": error_key, **details}


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class Gateway:
    def __init__(self, url, token_file):
        parsed = urllib.parse.urlsplit(url)
        try:
            local = ipaddress.ip_address(parsed.hostname or "").is_loopback
            port = parsed.port
        except ValueError:
            local, port = False, None
        if (not local or parsed.scheme != "http" or not port or parsed.username
                or parsed.password or parsed.query or parsed.fragment):
            raise BridgeError("gateway_must_be_an_explicit_loopback_http_url")
        self.url = url.rstrip("/")
        self.token_file = Path(token_file)
        self.http = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())

    def token(self):
        # Resolve credential material at use, including after rotation. Never
        # expose it as an MCP argument, response, environment value or log.
        descriptor = os.open(self.token_file, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor, "r", encoding="utf-8") as source:
            info = os.fstat(source.fileno())
            if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid()
                    or stat.S_IMODE(info.st_mode) & 0o077):
                raise BridgeError("token_file_requires_owner_only_regular_file")
            token = source.read(4097).strip()
        if not token or len(token) > 4096 or "\n" in token or "\r" in token:
            raise BridgeError("invalid_token_file")
        return token

    def request(self, method, path, body=None):
        payload = None if body is None else json.dumps(body).encode("utf-8")
        if payload is not None and len(payload) > MAX_MESSAGE:
            raise BridgeError("request_too_large")
        request = urllib.request.Request(
            self.url + path, data=payload, method=method,
            headers={"Authorization": "Bearer " + self.token(), "Content-Type": "application/json"},
        )
        try:
            with self.http.open(request, timeout=30) as response:
                encoded = response.read(MAX_RESPONSE + 1)
            if len(encoded) > MAX_RESPONSE:
                raise BridgeError("gateway_response_too_large", outcome_unknown=method != "GET")
            return json.loads(encoded)
        except urllib.error.HTTPError as error:
            # Server errors can contain submitted values. Return only stable
            # status/code metadata; the schema/list operation provides guidance.
            code = None
            try:
                detail = json.loads(error.read(MAX_RESPONSE))
                candidate = detail.get("code") or detail.get("error", {})
                if isinstance(candidate, dict):
                    candidate = candidate.get("code")
                if isinstance(candidate, str) and re.fullmatch(r"[a-z_]{1,80}", candidate):
                    code = candidate
            except (ValueError, AttributeError, OSError, http.client.HTTPException):
                pass
            raise BridgeError("gateway_rejected_request", status=error.code, code=code,
                              outcome_unknown=method != "GET" and (error.code == 408 or error.code >= 500)) from None
        except (urllib.error.URLError, TimeoutError, ConnectionError, http.client.HTTPException, json.JSONDecodeError):
            # Do not replay mutations: a lost response is not proof of refusal.
            raise BridgeError("gateway_request_failed", outcome_unknown=method != "GET") from None


def tool(name, description, properties=None, required=None, read_only=False):
    return {
        "name": name, "description": description,
        "inputSchema": {"type": "object", "properties": properties or {},
                        "required": required or [], "additionalProperties": False},
        "annotations": {"readOnlyHint": read_only, "openWorldHint": False},
    }


TOOLS = [
    tool("list", "Discover configuration paths, current values, types and enum choices. Secrets are masked. Use a prefix such as agents or runtime_profiles; paginate with offset.",
         {"prefix": {"type": "string"}, "offset": {"type": "integer", "minimum": 0},
          "limit": {"type": "integer", "minimum": 1, "maximum": 100}}, read_only=True),
    tool("get", "Read one configuration property by its dotted path. Secret values are never returned.",
         {"path": {"type": "string", "minLength": 1}}, ["path"], True),
    tool("create_alias", "Create a named configuration entry. During an active conversation the request is queued, not saved yet; finish your reply so the worker can apply it after idle. Check status in a later turn before setting fields on a queued new alias.",
         {"path": {"type": "string", "minLength": 1}, "key": {"type": "string", "minLength": 1}}, ["path", "key"]),
    tool("set", "Set one typed leaf configuration property discovered with list. For named entries such as MCP servers, use create_alias first, then set individual fields; do not assign a whole server object or JSON string to its alias path. If any native conversation is running, this returns queued=true, saved=false and a job id: finish your reply so the change can run after idle. Do not wait or poll for it inside the same turn. Check status/read back in a later turn. Request reload separately if needed. Never blindly repeat an uncertain write.",
         {"path": {"type": "string", "minLength": 1}, "value": {}}, ["path", "value"]),
    tool("patch", "Submit an atomic configuration patch. Paths may be dotted or JSON Pointer; use test operations for concurrency protection. During an active conversation this returns queued=true, saved=false: finish your reply and inspect status in a later turn. Do not poll inside the requesting turn. Validation occurs when the queued patch runs; rejected batches change nothing. Never blindly retry an uncertain write.",
         {"operations": {"type": "array", "minItems": 1, "maxItems": 256, "items": {
             "type": "object", "properties": {"op": {"type": "string", "enum": ["add", "replace", "remove", "test"]},
             "path": {"type": "string", "minLength": 1}, "value": {}}, "required": ["op", "path"], "additionalProperties": False}}}, ["operations"]),
    tool("status", "Inspect canonical application status and recent queued writes/reloads. A queued write is not saved yet. Finish the requesting turn before checking its completion in a later turn. An accepted reload does not prove every setting has been applied.", read_only=True),
    tool("reload", "Queue a daemon reload after all native sessions have been idle for 15 seconds. Return your final reply; the worker waits outside this tool call, then requests the same reload as the dashboard. Requests expire after five minutes if idle cannot be established. Connections may reconnect. Check status for the outcome."),
]


def write_job(path, body):
    path = Path(path)
    descriptor, temporary = tempfile.mkstemp(prefix=".reload-", dir=path.parent)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as target:
            json.dump(body, target)
            target.flush()
            os.fsync(target.fileno())
        os.replace(temporary, path)
        if body.get("state") not in ("waiting_for_idle", "applying"):
            prune_completed_jobs(path.parent)
    finally:
        Path(temporary).unlink(missing_ok=True)


def sessions_busy(gateway):
    running = gateway.request("GET", "/api/sessions/running?activity_only=true")
    # Missing on older daemons: their session list only covers WebSocket
    # persistence and cannot establish idle for channel or RPC callers.
    if not isinstance(running, dict) or type(running.get("busy")) is not bool:
        raise BridgeError("native_session_idle_detection_unavailable")
    return running["busy"]


def job_records(directory):
    records = []
    for pattern in ("write-*.json", "reload-*.json"):
        for path in Path(directory).glob(pattern):
            try:
                records.append((path, json.loads(path.read_text())))
            except FileNotFoundError:
                continue  # A concurrent worker may prune completed records.
    return records


def prune_completed_jobs(directory):
    records = [(path, job) for path, job in job_records(directory)
               if job.get("state") not in ("waiting_for_idle", "applying")]
    records.sort(key=lambda entry: entry[1].get("completed_at", entry[1].get("requested_at", 0)),
                 reverse=True)
    cutoff = time.time() - COMPLETED_JOB_MAX_AGE
    for index, (path, job) in enumerate(records):
        if (index >= COMPLETED_JOB_LIMIT
                or job.get("completed_at", job.get("requested_at", 0)) < cutoff):
            path.unlink(missing_ok=True)


def pending_writes(directory, before=None):
    prune_completed_jobs(directory)
    for path, job in job_records(directory):
        if not path.name.startswith("write-"):
            continue
        if (job.get("state") in ("waiting_for_idle", "applying")
                and time.time() - job["requested_at"] < 360
                and (before is None or (job["requested_at"], job["id"]) < before)):
            return True
    return False


def run_write(gateway, job_path, request, idle_seconds=15, deadline_seconds=300):
    """Payload stays in the worker's memory, never in job files or argv."""
    job = json.loads(Path(job_path).read_text())
    deadline, idle_since = time.monotonic() + deadline_seconds, None
    try:
        while time.monotonic() < deadline:
            earlier = pending_writes(Path(job_path).parent, (job["requested_at"], job["id"]))
            try:
                busy = sessions_busy(gateway)
            except BridgeError as error:
                if error.details["error"] != "gateway_request_failed" and error.details.get("status") != 503:
                    raise
                idle_since = None
                time.sleep(1)
                continue  # Retry only this read probe across a daemon reload.
            if busy:
                idle_since = None
            elif idle_since is None:
                idle_since = time.monotonic()
            elif not earlier and time.monotonic() - idle_since >= idle_seconds:
                job.update(state="applying")
                write_job(job_path, job)
                gateway.request(request["method"], request["path"], request["body"])
                job.update(state="saved", completed_at=time.time())
                write_job(job_path, job)
                return
            time.sleep(1)
        raise BridgeError("write_idle_deadline_expired")
    except BridgeError as error:
        job.update(state="failed", completed_at=time.time(), **error.details)
        write_job(job_path, job)
    except (OSError, ValueError):
        job.update(state="failed", completed_at=time.time(), error="write_worker_io_failed")
        write_job(job_path, job)


def run_reload(gateway, job_path, idle_seconds=15, deadline_seconds=300):
    """The job records a request; daemon application status remains authoritative."""
    job = json.loads(Path(job_path).read_text())
    deadline = time.monotonic() + deadline_seconds
    idle_since = None
    try:
        while time.monotonic() < deadline:
            if sessions_busy(gateway) or pending_writes(Path(job_path).parent):
                idle_since = None
            elif idle_since is None:
                idle_since = time.monotonic()
            elif time.monotonic() - idle_since >= idle_seconds:
                gateway.request("POST", "/admin/reload")
                job.update(state="reload_requested", completed_at=time.time())
                write_job(job_path, job)
                return
            time.sleep(1)
        raise BridgeError("reload_idle_deadline_expired")
    except BridgeError as error:
        job.update(state="failed", completed_at=time.time(), **error.details)
        write_job(job_path, job)
    except (OSError, ValueError):
        job.update(state="failed", completed_at=time.time(), error="reload_worker_io_failed")
        write_job(job_path, job)


class Control:
    def __init__(self, gateway, state_dir):
        self.gateway = gateway
        self.state_dir = Path(state_dir)

    def queue_write(self, method, path, body, paths):
        self.state_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
        job = {"id": uuid.uuid4().hex, "state": "waiting_for_idle", "kind": "write",
               "requested_at": time.time(), "paths": paths}
        job_path = self.state_dir / ("write-" + job["id"] + ".json")
        write_job(job_path, job)
        payload = json.dumps({"method": method, "path": path, "body": body}).encode()
        if len(payload) > MAX_MESSAGE:
            job.update(state="failed", error="request_too_large")
            write_job(job_path, job)
            raise BridgeError("request_too_large")
        try:
            worker = subprocess.Popen(
                [sys.executable, str(Path(__file__).resolve()), "--gateway-url", self.gateway.url,
                 "--token-file", str(self.gateway.token_file.resolve()), "--state-dir", str(self.state_dir.resolve()),
                 "--write-job", str(job_path.resolve())], stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL, start_new_session=True, close_fds=True)
            worker.stdin.write(payload)
            worker.stdin.close()
        except OSError:
            job.update(state="failed", error="write_worker_start_failed")
            write_job(job_path, job)
            raise BridgeError("write_worker_start_failed") from None
        return {"queued": True, "saved": False, "job": job,
                "next_step": "Finish this reply. Inspect status in a later turn; do not poll within this turn."}

    def call(self, name, arguments):
        spec = next((item for item in TOOLS if item["name"] == name), None)
        if spec is None:
            raise BridgeError("unknown_tool")
        schema = spec["inputSchema"]
        if not isinstance(arguments, dict) or set(arguments) - set(schema["properties"]):
            raise BridgeError("invalid_arguments")
        if any(key not in arguments for key in schema["required"]):
            raise BridgeError("missing_argument")
        for key in ("path", "prefix", "key"):
            if key in arguments and (not isinstance(arguments[key], str) or (key != "prefix" and not arguments[key].strip())):
                raise BridgeError("invalid_property_path")
        if name == "list":
            offset, limit = arguments.get("offset", 0), arguments.get("limit", 40)
            if type(offset) is not int or offset < 0 or type(limit) is not int or not 1 <= limit <= 100:
                raise BridgeError("invalid_pagination")
            query = urllib.parse.urlencode({"prefix": arguments.get("prefix", "")})
            data = self.gateway.request("GET", "/api/config/list?" + query)
            entries = data["entries"]
            return {"entries": entries[offset:offset + limit], "total": len(entries),
                    "next_offset": offset + limit if offset + limit < len(entries) else None}
        if name == "get":
            return self.gateway.request("GET", "/api/config/prop?" + urllib.parse.urlencode(arguments))
        if name == "create_alias":
            path = "/api/config/map-key?" + urllib.parse.urlencode(arguments)
            if sessions_busy(self.gateway) or pending_writes(self.state_dir):
                return self.queue_write("POST", path, None, [arguments["path"] + "." + arguments["key"]])
            self.gateway.request("POST", path)
            return {"created": True, "path": arguments["path"], "key": arguments["key"]}
        if name in ("set", "patch"):
            if name == "set":
                operations = [{"op": "add", "path": arguments["path"], "value": arguments["value"]}]
            else:
                operations = arguments["operations"]
            if not isinstance(operations, list) or not 1 <= len(operations) <= 256:
                raise BridgeError("invalid_patch")
            for entry in operations:
                if (not isinstance(entry, dict) or set(entry) - {"op", "path", "value"}
                        or entry.get("op") not in ("add", "replace", "remove", "test")
                        or not isinstance(entry.get("path"), str) or not entry["path"].strip()
                        or (entry["op"] != "remove" and "value" not in entry)):
                    raise BridgeError("invalid_patch")
            paths = [entry["path"] for entry in operations]
            if sessions_busy(self.gateway) or pending_writes(self.state_dir):
                return self.queue_write("PATCH", "/api/config", operations, paths)
            self.gateway.request("PATCH", "/api/config", operations)
            # Never echo submitted values, even if a gateway response does.
            result = {"saved": True, "paths": [entry["path"] for entry in operations]}
            try:
                result["application"] = self.gateway.request("GET", "/api/config/reload-status")
            except (BridgeError, OSError):
                result["application_status_unavailable"] = True
            return result
        if name == "status":
            prune_completed_jobs(self.state_dir)
            jobs = sorted((job for _, job in job_records(self.state_dir)),
                          key=lambda job: job.get("requested_at", 0), reverse=True)[:20]
            return {"application": self.gateway.request("GET", "/api/config/reload-status"),
                    "requests": jobs}
        if name == "reload":
            self.state_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
            job = {"id": uuid.uuid4().hex, "state": "waiting_for_idle", "requested_at": time.time()}
            path = self.state_dir / ("reload-" + job["id"] + ".json")
            write_job(path, job)
            try:
                subprocess.Popen([sys.executable, str(Path(__file__).resolve()),
                                  "--gateway-url", self.gateway.url, "--token-file", str(self.gateway.token_file.resolve()),
                                  "--state-dir", str(self.state_dir.resolve()), "--reload-job", str(path.resolve())],
                                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                 start_new_session=True, close_fds=True)
            except OSError:
                job.update(state="failed", error="reload_worker_start_failed")
                write_job(path, job)
                raise BridgeError("reload_worker_start_failed") from None
            return job
        raise BridgeError("unknown_tool")


def serve(control, source=sys.stdin.buffer, target=sys.stdout):
    initialized = False
    while True:
        line = source.readline(MAX_MESSAGE + 1)
        if not line:
            return
        if len(line) > MAX_MESSAGE:
            return  # Fail closed; do not interpret a tail as another request.
        identifier = None
        try:
            request = json.loads(line)
            if not isinstance(request, dict) or request.get("jsonrpc") != "2.0":
                raise BridgeError("invalid_request")
            identifier = request.get("id")
            if "id" not in request:
                continue
            method, params = request.get("method"), request.get("params", {})
            if not isinstance(params, dict):
                raise BridgeError("invalid_arguments")
            if method == "initialize" and not initialized:
                version = params.get("protocolVersion")
                initialized = True
                result = {"protocolVersion": version if version in SUPPORTED_VERSIONS else SUPPORTED_VERSIONS[-1],
                          "capabilities": {"tools": {}}, "serverInfo": {"name": "zeroclaw-config-control", "version": "0.1.0"}}
            elif not initialized:
                raise BridgeError("initialize_required")
            elif method == "ping":
                result = {}
            elif method == "tools/list":
                result = {"tools": TOOLS}
            elif method == "tools/call":
                try:
                    value = control.call(params.get("name"), params.get("arguments", {}))
                    failed = False
                except BridgeError as error:
                    value, failed = error.details, True
                except (OSError, ValueError, KeyError, TypeError):
                    value, failed = {"error": "configuration_control_failed"}, True
                result = {"content": [{"type": "text", "text": json.dumps(value)}], "isError": failed}
            else:
                raise BridgeError("unknown_method")
            response = {"jsonrpc": "2.0", "id": identifier, "result": result}
        except (ValueError, BridgeError) as error:
            code = -32700 if isinstance(error, ValueError) else -32602
            if isinstance(error, BridgeError) and str(error) == "unknown_method":
                code = -32601
            response = {"jsonrpc": "2.0", "id": identifier,
                        "error": {"code": code, "message": "Invalid MCP request"}}
        target.write(json.dumps(response) + "\n")
        target.flush()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gateway-url", required=True, help="Explicit loopback gateway URL, including any path prefix")
    parser.add_argument("--token-file", required=True, help="Owner-only file containing a dedicated paired-device token")
    parser.add_argument("--state-dir", required=True, help="Private directory for deferred reload request records")
    workers = parser.add_mutually_exclusive_group()
    workers.add_argument("--reload-job", help=argparse.SUPPRESS)
    workers.add_argument("--write-job", help=argparse.SUPPRESS)
    args = parser.parse_args()
    os.umask(0o077)
    gateway = Gateway(args.gateway_url, args.token_file)
    if args.write_job:
        data = sys.stdin.buffer.read(MAX_MESSAGE + 1)
        if len(data) > MAX_MESSAGE:
            raise BridgeError("request_too_large")
        run_write(gateway, args.write_job, json.loads(data))
    elif args.reload_job:
        run_reload(gateway, args.reload_job)
    else:
        serve(Control(gateway, args.state_dir))


if __name__ == "__main__":
    try:
        main()
    except (BridgeError, OSError, ValueError):
        print("Configuration bridge could not start; check local endpoint and private files.", file=sys.stderr)
        sys.exit(1)
