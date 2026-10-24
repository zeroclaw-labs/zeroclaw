"""Transport helper unit tests; run with the XMPP requirements installed."""
import importlib.util
import io
import asyncio
import threading
from unittest.mock import patch
import json
from pathlib import Path
import tempfile
import unittest
from slixmpp import JID

spec=importlib.util.spec_from_file_location('bridge',Path(__file__).parent.parent/'src/xmpp_bridge.py')
bridge=importlib.util.module_from_spec(spec)
spec.loader.exec_module(bridge)


class BridgeTests(unittest.TestCase):
    def test_maximum_unicode_message_fits_worker_frame_and_next_frame_survives(self):
        for character in ["\U0001f680", "\x01"]:
            body = character * 64000
            raw = io.BytesIO()
            # The process locale must not determine the protocol encoding.
            output = io.TextIOWrapper(raw, encoding="ascii")
            with patch.object(bridge.sys, "stdout", output):
                asyncio.run(bridge.emit("message", id="one", **{"from": "alice@example.org"},
                            body=body, encrypted=True))
                asyncio.run(bridge.emit("ready"))
            frames = raw.getvalue().splitlines()
            self.assertEqual(len(frames), 2)
            self.assertLess(len(frames[0]), 512 * 1024)
            self.assertEqual(json.loads(frames[0])["body"], body)
            self.assertEqual(json.loads(frames[1])["type"], "ready")

    def test_blocked_stdout_does_not_block_event_loop(self):
        entered = threading.Event()
        release = threading.Event()
        class Output:
            def write(self, data):
                entered.set()
                if not release.wait(3):
                    raise TimeoutError("test writer not released")
            def flush(self): pass
        async def check():
            with patch.object(bridge.sys, "stdout", type("Stream", (), {"buffer": Output()})()):
                task = asyncio.create_task(bridge.emit("message", body="x" * 300000))
                try:
                    self.assertTrue(await asyncio.to_thread(entered.wait, 1))
                    await asyncio.wait_for(asyncio.sleep(0), .2)
                    self.assertFalse(task.done())
                finally:
                    release.set()
                    await task
        asyncio.run(check())

    def test_grouped_fingerprints_and_invalid_entries(self):
        fingerprint='0123456789abcdef'*4
        grouped=' '.join(fingerprint[i:i+8] for i in range(0,64,8))
        pins=bridge.parse_pins(['alice@example.org '+grouped],JID)
        self.assertIn(('alice@example.org',bytes.fromhex(fingerprint)),pins)
        for entry in ['alice@example.org/device '+fingerprint,'alice@example.org bad','example.org '+fingerprint]:
            with self.assertRaises((ValueError,TypeError)):
                bridge.parse_pins([entry],JID)

    def test_sqlite_persists_and_refuses_final_symlink(self):
        with tempfile.TemporaryDirectory() as root:
            path=Path(root)/'state'
            db=bridge.private_database(path)
            with db:db.execute('insert into state values (?,?)',('fixture',json.dumps('persistent')))
            db.close()
            db=bridge.private_database(path)
            self.assertEqual(json.loads(db.execute("select value from state where key='fixture'").fetchone()[0]),'persistent')
            db.close()
            self.assertEqual((path/'omemo.sqlite3').stat().st_mode & 0o777,0o600)
            link=Path(root)/'link';link.symlink_to(path,target_is_directory=True)
            with self.assertRaises(ValueError):bridge.private_database(link)


if __name__=='__main__':unittest.main()
