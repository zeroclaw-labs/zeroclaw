"""Private JSON-lines transport worker for the optional XMPP channel.

Invoked by the Rust adapter using an isolated interpreter, never a shell.
Protocol and cryptography belong to Slixmpp/slixmpp-omemo. No agent logic here.
"""
import asyncio
import json
import logging
import os
from pathlib import Path
import sqlite3
import ssl
import sys
import threading
import uuid


_output_lock = threading.Lock()


def write_frame(kind, **fields):
    # Write UTF-8 explicitly: ASCII escaping can exceed the Rust frame bound
    # for an otherwise valid maximum-length Unicode message.
    frame = json.dumps({"type": kind, **fields}, ensure_ascii=False) + "\n"
    with _output_lock:
        sys.stdout.buffer.write(frame.encode("utf-8"))
        sys.stdout.buffer.flush()


async def emit(kind, **fields):
    # A full stdout pipe must not stop XMPP handlers or stdin consumption.
    await asyncio.to_thread(write_frame, kind, **fields)


def private_database(directory):
    directory = Path(directory)
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    if directory.is_symlink() or not directory.is_dir():
        raise ValueError("Invalid state directory")
    if hasattr(os, "getuid") and directory.stat().st_uid != os.getuid():
        raise ValueError("State directory belongs to another user")
    directory.chmod(0o700)
    path = directory / "omemo.sqlite3"
    if path.is_symlink():
        raise ValueError("Invalid state database")
    fd = os.open(path, os.O_CREAT | os.O_RDWR | getattr(os, "O_NOFOLLOW", 0), 0o600)
    os.close(fd)
    if hasattr(os, "getuid") and path.stat().st_uid != os.getuid():
        raise ValueError("State database belongs to another user")
    os.chmod(path, 0o600)
    db = sqlite3.connect(path)
    db.execute("create table if not exists state (key text primary key, value text not null)")
    db.execute("create table if not exists seen (sender text, id text, primary key(sender,id))")
    return db


def parse_pins(entries, jid_class):
    result = set()
    for entry in entries:
        jid, *groups = entry.split()
        fingerprint = "".join(groups)
        parsed = jid_class(jid)
        if not parsed.user or parsed.resource:
            raise ValueError("Fingerprint identity must be a bare account JID")
        raw = bytes.fromhex(fingerprint)
        if len(raw) == 33 and raw[0] == 5:
            raw = raw[1:]
        if len(raw) != 32:
            raise ValueError("Invalid identity key length")
        result.add((parsed.bare, raw))
    return result


async def main(setup):
    from slixmpp import ClientXMPP, JID
    from slixmpp.xmlstream.handler import CoroutineCallback
    from slixmpp.xmlstream.matcher import MatchXPath

    cfg = setup["config"]
    jid = JID(cfg["username"])
    if not jid.user or jid.resource:
        raise ValueError("Bare account JID required")
    if cfg["protocol"] not in ("direct_tls", "starttls"):
        raise ValueError("Encrypted transport required")
    db = private_database(setup["storage"])
    old = db.execute("select value from state where key='account'").fetchone()
    if old is not None and json.loads(old[0]) != jid.bare:
        raise ValueError("OMEMO state belongs to a different account")
    with db:
        db.execute("insert or replace into state values ('account',?)", (json.dumps(jid.bare),))
    pins = parse_pins(cfg["omemo_trusted_devices"], JID) if cfg["omemo_enabled"] else set()
    plugin_class = None
    if cfg["omemo_enabled"]:
        import xeddsa
        from omemo.storage import Storage, Just, Nothing
        from slixmpp_omemo import XEP_0384, TrustLevel

        def device_is_pinned(device):
            # OMEMO clients display Curve25519 fingerprints, whereas this
            # library's internal DeviceInformation key is Ed25519.
            fingerprint = xeddsa.ed25519_pub_to_curve25519_pub(device.identity_key)
            return (device.bare_jid, fingerprint) in pins

        class SqliteStorage(Storage):
            async def _load(self, key):
                row = db.execute("select value from state where key=?", ("omemo/" + key,)).fetchone()
                return Nothing() if row is None else Just(json.loads(row[0]))

            async def _store(self, key, value):
                with db:
                    db.execute("insert or replace into state values (?,?)", ("omemo/" + key, json.dumps(value)))

            async def _delete(self, key):
                with db:
                    db.execute("delete from state where key=?", ("omemo/" + key,))

        class PinnedOmemo(XEP_0384):
            @property
            def storage(self):
                return self._persistent_storage

            def plugin_init(self):
                self._persistent_storage = SqliteStorage()
                super().plugin_init()

            @property
            def _btbv_enabled(self):
                return False

            async def _prompt_manual_trust(self, devices, identifier):
                manager = await self.get_session_manager()
                for device in devices:
                    trusted = device_is_pinned(device)
                    await manager.set_trust(device.bare_jid, device.identity_key,
                                            TrustLevel.TRUSTED.value if trusted else TrustLevel.DISTRUSTED.value)

        plugin_class = PinnedOmemo

    done = asyncio.get_running_loop().create_future()
    tls = ssl.create_default_context()
    tls.minimum_version = ssl.TLSVersion.TLSv1_2

    class Client(ClientXMPP):
        def __init__(self):
            super().__init__(jid.bare + "/" + cfg["resource"], cfg["password"], ssl_context=tls)
            self.enable_direct_tls = cfg["protocol"] == "direct_tls"
            self.enable_starttls = cfg["protocol"] == "starttls"
            self.enable_plaintext = False
            self.add_event_handler("session_start", self.started)
            self.add_event_handler("failed_auth", self.failed)
            self.add_event_handler("connection_failed", self.failed)
            self.add_event_handler("disconnected", self.failed)
            self.register_handler(CoroutineCallback("Direct messages", MatchXPath("{jabber:client}message"), self.incoming))
            self.register_plugin("xep_0199")
            if plugin_class is not None:
                from slixmpp.plugins import register_plugin
                register_plugin(plugin_class)
                self.register_plugin("xep_0380")
                self.register_plugin("xep_0384", module=sys.modules[__name__])

        async def started(self, _):
            # Never authenticate or announce readiness over a plaintext transport.
            if self.transport.get_extra_info("ssl_object") is None:
                self.failed(None)
                return
            if cfg["omemo_enabled"]:
                manager = await self["xep_0384"].get_session_manager()
                own, _ = await manager.get_own_device_information()
                public = {"jid": own.bare_jid, "device_id": own.device_id,
                          "fingerprint": "".join(manager.format_identity_key(own.identity_key))}
                with db:
                    db.execute("insert or replace into state values ('public/identity',?)", (json.dumps(public),))
            self.send_presence()
            await emit("online")

        def failed(self, _):
            if not done.done():
                done.set_result(None)

        async def incoming(self, stanza):
            if stanza["type"] not in ("chat", "normal") or stanza["from"].bare == jid.bare:
                return
            # Never replay archive/backfill, sent carbons or forwarded envelopes.
            if any(child.tag in {"{urn:xmpp:delay}delay", "{urn:xmpp:carbons:2}sent",
                                 "{urn:xmpp:carbons:2}received", "{urn:xmpp:forward:0}forwarded"}
                   for child in stanza.xml):
                return
            encrypted = False
            from_jid = stanza["from"].bare
            if cfg["omemo_enabled"]:
                if not self["xep_0384"].is_encrypted(stanza):
                    return
                try:
                    plugin = self["xep_0384"]
                    manager = await plugin.get_session_manager()
                    # Previously distrusted devices remain distrusted in storage
                    # after a pin is added. Reconcile before decryption, which
                    # otherwise rejects them before the fingerprint check below.
                    for known in await manager.get_device_information(from_jid):
                        trusted = device_is_pinned(known)
                        await manager.set_trust(known.bare_jid, known.identity_key,
                                                TrustLevel.TRUSTED.value if trusted else TrustLevel.DISTRUSTED.value)
                    stanza, device = await plugin.decrypt_message(stanza)
                    if device.bare_jid != from_jid or not device_is_pinned(device):
                        return
                    encrypted = True
                except Exception:
                    return
            elif any("encrypted" in child.tag or child.tag == "{urn:xmpp:eme:0}encryption" for child in stanza.xml):
                return
            body = str(stanza["body"])
            if not body.strip() or len(body) > cfg["max_message_chars"]:
                return
            message_id = str(stanza["id"]) or str(uuid.uuid4())
            with db:
                inserted = db.execute("insert or ignore into seen values (?,?)", (from_jid, message_id)).rowcount
                db.execute("delete from seen where rowid not in (select rowid from seen order by rowid desc limit 2048)")
            if inserted:
                await emit("message", id=message_id, **{"from": from_jid}, body=body, encrypted=encrypted)

        async def send_text(self, command):
            target = JID(command["to"])
            if not target.user or target.resource or len(command["body"]) > cfg["max_message_chars"]:
                raise ValueError("Invalid outgoing message")
            message = self.make_message(mto=target, mtype="chat", mbody=command["body"])
            if cfg["omemo_enabled"]:
                plugin = self["xep_0384"]
                manager = await plugin.get_session_manager()
                # Reconcile persisted trust each send. Removing a pin then
                # reloading must revoke a previously trusted device as well.
                await plugin.refresh_device_lists({JID(target.bare), JID(self.boundjid.bare)})
                for account in {target.bare, self.boundjid.bare}:
                    for device in await manager.get_device_information(account):
                        trusted = device_is_pinned(device)
                        await manager.set_trust(device.bare_jid, device.identity_key,
                                                TrustLevel.TRUSTED.value if trusted else TrustLevel.DISTRUSTED.value)
                message.set_from(self.boundjid)
                messages, errors = await plugin.encrypt_message(message, {target})
                message = messages.get("eu.siacs.conversations.axolotl")
                if message is None or errors or not plugin.is_encrypted(message):
                    raise ValueError("OMEMO could not encrypt for every required device")
                message.set_to(target)
                message.set_from(self.boundjid)
                message["type"] = "chat"
                message["eme"]["namespace"] = "eu.siacs.conversations.axolotl"
                message["eme"]["name"] = "OMEMO"
            message.send()

    # The plugin registry imports this module attribute, not the closure name.
    if plugin_class is not None:
        globals()["XEP_0384"] = plugin_class
    client = Client()
    client.connect(cfg["server"], cfg["port"])

    async def commands():
        reader = asyncio.StreamReader(limit=512 * 1024)
        protocol = asyncio.StreamReaderProtocol(reader)
        await asyncio.get_running_loop().connect_read_pipe(lambda: protocol, sys.stdin)
        while line := await reader.readline():
            command = json.loads(line)
            if command.get("type") != "send":
                raise ValueError("Unknown bridge command")
            try:
                await asyncio.wait_for(client.send_text(command), timeout=45)
                await emit("sent", id=command["id"], ok=True)
            except Exception as error:
                await emit("sent", id=command["id"], ok=False, error_code=type(error).__name__)

    command_task = asyncio.create_task(commands())
    try:
        await asyncio.wait({done, command_task}, return_when=asyncio.FIRST_COMPLETED)
    finally:
        command_task.cancel()
        await client.disconnect()
        db.close()


if __name__ == "__main__":
    # No stanza/debug logging: it may contain credentials, message bodies or keys.
    logging.disable(logging.CRITICAL)
    os.umask(0o077)
    try:
        initial = sys.stdin.buffer.readline(512 * 1024)
        if not initial.endswith(b"\n"):
            raise ValueError("Invalid bridge setup")
        asyncio.run(main(json.loads(initial)))
    except Exception:
        write_frame("fatal")
        sys.exit(1)
