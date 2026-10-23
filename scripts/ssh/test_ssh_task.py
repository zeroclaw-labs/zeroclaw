import importlib.machinery
import importlib.util
import pathlib
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

    def test_aliases_share_transport_but_identity_routes_do_not(self):
        from types import SimpleNamespace
        config = 'hostname host.example.test\nport 22\nuser operator\nidentityagent /private/agent\nidentityfile key-a.pub\nidentityfile key-b.pub\n'
        with patch.object(module.subprocess, 'run', return_value=SimpleNamespace(stdout=config)):
            first = module.ssh_config([], 'alias-a')
            self.assertEqual(first, module.ssh_config([], 'alias-b'))
        with patch.object(module.subprocess, 'run', return_value=SimpleNamespace(stdout=config.replace('key-a.pub', 'different.pub'))):
            self.assertNotEqual(first, module.ssh_config([], 'alias-a'))


if __name__ == '__main__': unittest.main()
