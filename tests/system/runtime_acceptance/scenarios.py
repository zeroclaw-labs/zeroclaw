"""User journeys against binaries, sockets, HTTP routes and the real TUI."""

import json
import time

from support import Installation, RpcError, http, require, wait_for


def converse(install, terminal, label):
    prompt, response = "request-" + label, "REPLY_" + label.upper()
    install.model.enqueue(prompt, text="WRONG_RESPONSE" if install.fault == "response" else response)
    terminal.chat()
    terminal.send(prompt)
    terminal.expect(response)
    terminal.state("✓ acceptance")
    require(install.model.requests[-1].get("stream") is True, "conversation did not exercise streaming HTTP")
    install.model.verify()


def warm_startup(options, artifacts):
    with Installation(options.bin_dir, artifacts, fault=options.fault, timeout=options.timeout) as app:
        app.start()
        converse(app, app.tui(), "warm")


def cold_startup(options, artifacts):
    for legacy in (False, True):
        with Installation(options.bin_dir, artifacts / ("legacy" if legacy else "fresh"),
                          legacy=legacy, timeout=options.timeout) as app:
            require(not app.socket.exists(), "cold-start fixture already has a daemon")
            converse(app, app.tui(), "legacy" if legacy else "cold")
            require(app.socket.exists(), "ZeroCode did not start its daemon")


def tool_approval(options, artifacts):
    with Installation(options.bin_dir, artifacts, approval=True, timeout=options.timeout) as app:
        app.start()
        tui = app.tui()
        tui.chat()
        for name, key in (("approved", "Enter"), ("denied", "C-d")):
            output = app.workspace / (name + ".txt")
            prompt, reply = "write-" + name, "TOOL_" + name.upper()
            app.model.enqueue(prompt, tool=("file_write", {"path": name + ".txt", "content": "acceptance-data"}))
            app.model.enqueue("Written 15 bytes" if name == "approved" else "Denied by user", text=reply)
            tui.send(prompt)
            tui.expect("Approval Required")
            tui.state("⚠ acceptance")
            require(not output.exists(), "tool wrote the file before approval")
            tui.key(key)
            tui.expect(reply)
            tui.state("✓ acceptance")
            if name == "approved":
                require(output.read_text() == "acceptance-data", "approved tool result differs")
            else:
                require(not output.exists(), "denied tool wrote a file")
            app.model.verify()


def sop_persistence(options, artifacts):
    with Installation(options.bin_dir, artifacts, fault=options.fault, timeout=options.timeout) as app:
        directory = app.config / "shared" / "sops" / "acceptance"
        directory.mkdir(parents=True)
        (directory / "SOP.toml").write_text('''[sop]
name = "acceptance"
description = "Acceptance checkpoint and file result"
version = "1.0.0"
execution_mode = "auto"
agent = "acceptance"
[[triggers]]
type = "manual"
''')
        (directory / "SOP.md").write_text('''# Acceptance

## Steps

1. **Write result**
   - kind: checkpoint
   - requires_confirmation: true
   - tools: file_write
   Write SOP_COMPLETE to sop-result.txt in the workspace.
''')
        app.cli("sop", "validate", "acceptance")
        app.start()
        client = app.rpc()
        run_id = client.call("sops/run", {"name": "acceptance"})["run_id"]

        def run():
            rows = client.call("sops/runs", {"sop": "acceptance"})["runs"]
            (artifacts / "sop-runs.json").write_text(app.redact(json.dumps(rows, indent=2)))
            return next((row for row in rows if row["run_id"] == run_id), None)

        wait_for(lambda: (run() or {}).get("status") == "waiting_approval", "SOP checkpoint", options.timeout)
        output = app.workspace / "sop-result.txt"
        require(not output.exists(), "SOP crossed the checkpoint without approval")
        if options.fault != "sop":
            app.model.enqueue("Write result", tool=("file_write", {"path": "sop-result.txt", "content": "SOP_COMPLETE"}))
            app.model.enqueue("Written 12 bytes", text="SOP_COMPLETE")
            client.call("sops/decide", {"name": "acceptance", "run_id": run_id, "decision": "approve"})
        wait_for(lambda: (run() or {}).get("status") == "completed", "SOP completion", options.timeout)
        require(output.read_text() == "SOP_COMPLETE", "SOP did not execute its planned tool")
        client.call("memory/store", {"key": "acceptance-memory", "content": "PERSISTED_MEMORY", "agent": "acceptance"})
        client.close()
        app.clients.remove(client)
        app.stop()
        app.start()
        client = app.rpc()
        require((run() or {}).get("status") == "completed", "completed SOP record lost after restart")
        record = client.call("memory/get", {"key": "acceptance-memory", "agent": "acceptance"})
        require(record["entry"]["content"] == "PERSISTED_MEMORY", "memory lost after restart")
        app.model.verify()


def native_auth(options, artifacts):
    with Installation(options.bin_dir, artifacts, timeout=options.timeout) as app:
        app.start()
        token = app.pair()
        path = app.gateway + "/api/config"
        require(http(path, headers={"Authorization": "Bearer " + token})[0] == 200, "paired HTTP access denied")
        for headers in ({}, {"Authorization": "Bearer invalid-acceptance-token"}):
            require(http(path, headers=headers)[0] == 401, "invalid HTTP credentials accepted")
        app.model.verify()


def remote_auth(options, artifacts):
    with Installation(options.bin_dir, artifacts, oidc=True, timeout=options.timeout) as app:
        # Saving a credential through the public CLI initializes the normal
        # secret store, also used to sign remote TUI identities.
        app.cli("config", "set", "--no-interactive", "providers.models.custom.acceptance.api_key", "acceptance-provisioned-key")
        app.start()
        native = app.pair()
        terminal = app.tui(token=native, remote=True)
        converse(app, terminal, "native_remote")
        terminal.close()
        enrolled = app.cli("oidc", "token", "acceptance").strip()
        require(enrolled.count(".") == 2, "OIDC enrollment stdout is not exactly a JWT")
        app.secrets.add(enrolled)
        terminal = app.tui(token=enrolled, provider="oidc.acceptance", remote=True)
        converse(app, terminal, "oidc_remote")
        require(app.issuer.calls["/token"] > 0 and app.issuer.calls["/jwks"] > 0, "OIDC wire flow was not exercised")
        require(not app.issuer.errors, "issuer protocol failed")


def oidc_rejection(options, artifacts):
    with Installation(options.bin_dir, artifacts, oidc=True, timeout=options.timeout) as app:
        app.start()
        good = app.issuer.token()
        # Reach the same production credential resolver over real local RPC.
        app.rpc(good, "oidc.acceptance").call("config/get", {"prop": "gateway.port"})
        invalid_signature = good.rsplit(".", 1)[0] + "." + ("A" * 342)
        app.secrets.add(invalid_signature)
        tokens = [app.issuer.token(exp=int(time.time()) - 3600), app.issuer.token(iss="https://invalid.example"),
                  app.issuer.token(aud="wrong-audience"), invalid_signature, app.issuer.token(groups=["unmapped"])]
        for token in tokens:
            try:
                app.rpc(token, "oidc.acceptance")
            except RpcError as error:
                require(error.error["code"] in (-32010, -32012), "unexpected OIDC rejection: " + str(error))
            else:
                raise AssertionError("invalid OIDC identity authenticated over RPC")
            headers = {"Authorization": "Bearer " + token, "x-zeroclaw-auth-provider": "oidc.acceptance"}
            require(http(app.gateway + "/api/config", headers=headers)[0] in (401, 403), "invalid OIDC identity admitted over HTTP")
            require(http(app.gateway + "/webhook", "POST", {"message": "write rejected.txt"}, headers)[0] in (401, 403),
                    "invalid OIDC identity admitted to an agent turn")
        require(not app.model.requests, "rejected identities reached the model")
        require(not list(app.workspace.glob("*.txt")), "rejected identities produced a tool side effect")
        app.model.verify()


def live_authorization(options, artifacts):
    with Installation(options.bin_dir, artifacts, oidc=True, timeout=options.timeout) as app:
        app.start()
        operator = app.rpc()
        native = app.pair()
        token = app.issuer.token()
        principal = app.rpc(token, "oidc.acceptance")
        headers = {"Authorization": "Bearer " + token, "x-zeroclaw-auth-provider": "oidc.acceptance"}
        prop = "permission_profiles.acceptance.admin"
        require(http(app.gateway + "/api/config", headers=headers)[0] == 200, "initial OIDC access denied")
        # Change the verified identity's profile using the live config API.
        operator.call("config/set", {"prop": prop, "value": False})
        require(http(app.gateway + "/api/config", headers=headers)[0] == 403, "RPC revocation did not bind gateway")
        try:
            principal.call("config/get", {"prop": "gateway.port"})
        except RpcError as error:
            require(error.error["code"] == -32010, "established OIDC session did not require reauthentication")
        else:
            raise AssertionError("OIDC connection survived an authorization generation change")
        operator.call("config/set", {"prop": prop, "value": True})
        principal = app.rpc(token, "oidc.acceptance")
        require(http(app.gateway + "/api/config", headers=headers)[0] == 200, "restored permission not visible")
        status, body = http(app.gateway + "/api/config/prop", "PUT", {"path": prop, "value": False},
                            {"Authorization": "Bearer " + native})
        require(status == 200, "HTTP authorization edit failed: " + str(body))
        try:
            principal.call("config/get", {"prop": "gateway.port"})
        except RpcError as error:
            require(error.error["code"] == -32010, "HTTP edit did not require OIDC reauthentication")
        else:
            raise AssertionError("HTTP revocation did not bind established RPC principal")
        try:
            app.rpc(token, "oidc.acceptance").call("config/get", {"prop": "gateway.port"})
        except RpcError as error:
            require(error.error["code"] == -32012, "fresh OIDC session failed outside permission enforcement")
        else:
            raise AssertionError("fresh RPC principal retained revoked grants")
        app.model.verify()


def local_with_oidc(options, artifacts):
    for warm in (True, False):
        with Installation(options.bin_dir, artifacts / ("warm" if warm else "cold"),
                          oidc=True, timeout=options.timeout) as app:
            if warm:
                app.start()
            converse(app, app.tui(), "local_oidc")


# This registry owns suite membership and the expected execution inventory.
SCENARIOS = [("core", warm_startup), ("core", cold_startup), ("core", tool_approval),
             ("core", sop_persistence), ("core", native_auth), ("full", remote_auth),
             ("full", oidc_rejection), ("full", live_authorization), ("full", local_with_oidc)]
