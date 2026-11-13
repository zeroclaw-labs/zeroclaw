import concurrent.futures
import importlib.machinery
import importlib.util
import pathlib
import io
import threading
import time
import tempfile
import unittest
from unittest.mock import patch

path = pathlib.Path(__file__).with_name('zeroclaw-ssh-task')
loader = importlib.machinery.SourceFileLoader('ssh_task', str(path))
spec = importlib.util.spec_from_loader(loader.name, loader)
module = importlib.util.module_from_spec(spec)
loader.exec_module(module)


class FakeMaster:
    starts = []
    fail = False

    def __init__(self, command, **kwargs):
        self.command, self.returncode = command, (255 if self.fail else None)
        self.stderr = io.BytesIO()
        self.starts.append(self)
        if not self.fail:
            pathlib.Path(command[command.index('-S') + 1]).touch()

    def poll(self): return self.returncode
    def terminate(self): self.returncode = 0
    def kill(self): self.returncode = -9
    def wait(self, timeout): return self.returncode


class TaskTransportTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.clock = 100.0
        self.pool = module.Pool(pathlib.Path(self.temp.name), clock=lambda: self.clock)
        FakeMaster.starts = []
        FakeMaster.fail = False
        self.mock = patch.object(module.subprocess, 'Popen', FakeMaster)
        self.mock.start()

    def tearDown(self):
        self.pool.close()
        self.mock.stop()
        self.temp.cleanup()

    def test_command_options_cannot_control_the_shared_transport(self):
        from unittest.mock import MagicMock
        for command in [['-O', 'exit'], ['-o', 'ControlMaster=yes']]:
            peer = MagicMock()
            with patch.dict(module.os.environ, {'ZEROCLAW_SSH_TASK': 'a' * 32}), \
                 patch.object(module.socket, 'socket', return_value=peer), \
                 patch.object(module, 'read_message', return_value={'path': '/synthetic/mux'}), \
                 patch.object(module.subprocess, 'call', return_value=0) as execute:
                self.assertEqual(module.client(['alias', *command]), 0)
                argv = execute.call_args.args[0]
                delimiter = argv.index('--')
                self.assertEqual(argv[delimiter + 1:], ['alias', *command])

    def test_scoped_signer_is_retired_before_master_is_leased(self):
        signer = object()
        with patch.object(module, 'start_scoped_agent', return_value=signer) as start, \
             patch.object(module, 'stop_agent') as stop:
            first = self.pool.acquire('scoped', [], 'host-a', ('local', 'dummy'))
            stop.assert_called_once_with(signer, self.pool.entries['scoped']['agent_path'])
            self.assertEqual(self.pool.acquire('scoped', [], 'host-a', ('local', 'dummy')), first)
            self.assertEqual(len(FakeMaster.starts), 1)
            start.assert_called_once()
            self.pool.close()
            stop.assert_called_once()

    def test_failed_scoped_authentication_retires_signer(self):
        FakeMaster.fail = True
        with patch.object(module, 'start_scoped_agent', return_value=object()), \
             patch.object(module, 'stop_agent') as stop:
            with self.assertRaisesRegex(RuntimeError, 'connection failed'):
                self.pool.acquire('scoped', [], 'host-a', ('local', 'dummy'))
            self.pool.close()
            stop.assert_called_once()

    def test_one_master_per_host_even_with_overlapping_commands(self):
        first = self.pool.acquire('same', [], 'host-a')
        self.assertEqual(self.pool.acquire('same', [], 'host-a'), first)
        self.pool.acquire('other', [], 'host-b')
        self.assertEqual(len(FakeMaster.starts), 2)
        self.assertEqual(self.pool.entries['same']['active'], 2)

    def test_idle_timeout_30_minutes_and_active_command_not_killed(self):
        self.pool.acquire('same', [], 'host')
        self.clock += 1801
        self.pool.expire()
        self.assertIsNone(FakeMaster.starts[0].poll())
        self.pool.release('same')
        self.clock += 1799
        self.pool.expire()
        self.assertIn('same', self.pool.entries)
        self.clock += 1
        self.pool.expire()
        self.assertEqual(self.pool.entries, {})
        self.assertEqual(FakeMaster.starts[0].poll(), 0)

    def test_healthy_idle_transport_can_be_recreated(self):
        self.pool.acquire('same', [], 'host')
        self.pool.release('same')
        self.clock += 1800
        self.pool.expire()
        self.pool.acquire('same', [], 'host')
        self.assertEqual(len(FakeMaster.starts), 2)
        self.assertEqual(FakeMaster.starts[0].poll(), 0)
        self.assertIsNone(FakeMaster.starts[1].poll())

    def test_failed_connect_remains_unavailable_after_idle_expiry(self):
        FakeMaster.fail = True
        with self.assertRaisesRegex(RuntimeError, 'not retried'):
            self.pool.acquire('same', [], 'host')
        entry = self.pool.entries['same']
        FakeMaster.fail = False
        for _ in range(2):
            self.clock += 1801
            self.pool.expire()
            with self.assertRaisesRegex(RuntimeError, 'unavailable'):
                self.pool.acquire('same', [], 'host')
            self.assertEqual(len(FakeMaster.starts), 1)
        self.assertTrue(entry['log'].closed)
        self.assertFalse(pathlib.Path(entry['path']).exists())
        self.pool.close()
        self.assertEqual(self.pool.entries, {})

    def test_dead_transport_remains_unavailable_after_idle_expiry(self):
        for reject_before_expiry in (False, True):
            with self.subTest(reject_before_expiry=reject_before_expiry):
                key = str(reject_before_expiry)
                self.pool.acquire(key, [], 'host')
                self.pool.release(key)
                entry = self.pool.entries[key]
                entry['process'].returncode = 255
                starts = len(FakeMaster.starts)
                if reject_before_expiry:
                    with self.assertRaisesRegex(RuntimeError, 'unavailable'):
                        self.pool.acquire(key, [], 'host')
                for _ in range(2):
                    self.clock += 1801
                    self.pool.expire()
                    with self.assertRaisesRegex(RuntimeError, 'unavailable'):
                        self.pool.acquire(key, [], 'host')
                    self.assertEqual(len(FakeMaster.starts), starts)
                self.assertTrue(entry['log'].closed)
                self.assertFalse(pathlib.Path(entry['path']).exists())

    def test_task_end_closes_all_hosts_immediately_and_rejects_new_commands(self):
        self.pool.acquire('one', [], 'host-a')
        self.pool.acquire('two', [], 'host-b')
        self.pool.close()
        self.assertTrue(all(p.poll() == 0 for p in FakeMaster.starts))
        with self.assertRaisesRegex(RuntimeError, 'task has ended'):
            self.pool.acquire('one', [], 'host-a')

    def test_failed_connect_is_not_retried_in_task(self):
        FakeMaster.fail = True
        with self.assertRaisesRegex(RuntimeError, 'not retried'):
            self.pool.acquire('one', [], 'host')
        with self.assertRaisesRegex(RuntimeError, 'unavailable'):
            self.pool.acquire('one', [], 'host')
        self.assertEqual(len(FakeMaster.starts), 1)

    def test_identity_trust_and_unmanaged_mux_overrides_rejected(self):
        for option in ['IdentityFile=x', 'StrictHostKeyChecking=no',
                       'ControlPath=x', 'ProxyCommand=x', 'IdentityAgent=x']:
            with self.assertRaises(ValueError):
                module.parse_args(['-o', option, 'siem', 'hostname'])
        options, host, commands = module.parse_args(
            ['-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10', 'siem', 'hostname'])
        self.assertEqual(host, 'siem')
        self.assertEqual(commands, ['hostname'])

    def test_aliases_and_identity_routes_have_separate_transports(self):
        from types import SimpleNamespace
        config = 'hostname host.example.test\nport 22\nuser operator\nidentityagent /private/agent\nidentityfile key-a.pub\nidentityfile key-b.pub\n'
        with patch.object(module.subprocess, 'run', return_value=SimpleNamespace(stdout=config)):
            first = module.ssh_config([], 'alias-a')
            self.assertNotEqual(first, module.ssh_config([], 'alias-b'))
        with patch.object(module.subprocess, 'run', return_value=SimpleNamespace(stdout=config.replace('key-a.pub', 'different.pub'))):
            self.assertNotEqual(first, module.ssh_config([], 'alias-a'))


    def test_trust_changes_never_reuse_a_permissive_master(self):
        from types import SimpleNamespace
        for field in ['stricthostkeychecking', 'userknownhostsfile', 'globalknownhostsfile',
                      'verifyhostkeydns', 'checkhostip', 'hostkeyalgorithms']:
            with self.subTest(field=field):
                with patch.object(module.subprocess, 'run', return_value=SimpleNamespace(stdout=field+' weak\n')):
                    weak = module.ssh_config([], 'same')
                with patch.object(module.subprocess, 'run', return_value=SimpleNamespace(stdout=field+' strict\n')):
                    self.assertNotEqual(weak, module.ssh_config([], 'same'))

    def test_stderr_is_drained_and_bounded(self):
        log = module.BoundedLog()
        log.attach(io.BytesIO(b'x' * 2_000_000 + b'END'))
        log.reader.join(timeout=2)
        self.assertFalse(log.reader.is_alive())
        self.assertLessEqual(len(log.data), log.LIMIT)
        self.assertTrue(log.detail().endswith('END'))
        log.close()
        self.assertTrue(log.closed)
        self.assertEqual(log.data, b'')

    def test_slow_destination_does_not_block_other_keys_or_cleanup(self):
        class SlowMaster(FakeMaster):
            def __init__(self, command, **kwargs):
                super().__init__(command, **kwargs)
                if command[-1] == 'slow':
                    pathlib.Path(command[command.index('-S') + 1]).unlink()
        errors = []
        def connect(key):
            try: self.pool.acquire(key, [], 'slow')
            except RuntimeError: errors.append(key)
        with patch.object(module.subprocess, 'Popen', SlowMaster):
            threads = [threading.Thread(target=connect, args=(key,)) for key in ['a','b']]
            for t in threads: t.start()
            deadline = time.monotonic()+2
            while len(FakeMaster.starts)<2 and time.monotonic()<deadline: time.sleep(.01)
            start = time.monotonic()
            self.pool.acquire('fast', [], 'fast')
            self.assertLess(time.monotonic()-start, 1)
            self.assertEqual(len(FakeMaster.starts), 3)
            self.pool.close()
            for t in threads: t.join(timeout=2)
            self.assertTrue(all(not t.is_alive() for t in threads))
            self.assertEqual(sorted(errors), ['a','b'])

    def test_overlapping_first_connections_for_one_key_spawn_only_once(self):
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as executor:
            results = list(executor.map(lambda _: self.pool.acquire('same', [], 'host'), range(8)))
        self.assertEqual(len(set(results)), 1)
        self.assertEqual(len(FakeMaster.starts), 1)



    def test_idle_retirement_finishes_before_a_new_generation_uses_the_path(self):
        self.pool.acquire('same', [], 'host')
        self.pool.release('same')
        self.clock += 1801
        stopping, proceed = threading.Event(), threading.Event()
        original_stop = self.pool.stop
        def paused_stop(entry):
            if entry.get('retired'):
                stopping.set()
                self.assertTrue(proceed.wait(timeout=3))
            original_stop(entry)
        results = []
        with patch.object(self.pool, 'stop', paused_stop):
            expiry = threading.Thread(target=self.pool.expire)
            expiry.start()
            self.assertTrue(stopping.wait(timeout=2))
            acquire = threading.Thread(target=lambda: results.append(self.pool.acquire('same', [], 'host')))
            acquire.start()
            try:
                time.sleep(.05)
                self.assertTrue(acquire.is_alive())
                self.assertEqual(len(FakeMaster.starts), 1)
            finally:
                proceed.set()
                expiry.join(timeout=2)
                acquire.join(timeout=2)
            self.assertFalse(expiry.is_alive())
            self.assertFalse(acquire.is_alive())
        self.assertEqual(len(FakeMaster.starts), 2)
        self.assertEqual(len(results), 1)
        self.assertTrue(pathlib.Path(results[0]).exists())
        self.assertEqual(self.pool.entries['same']['active'], 1)


if __name__ == '__main__': unittest.main()
