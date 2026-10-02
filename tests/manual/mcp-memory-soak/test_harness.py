import importlib.util
import json
import hashlib
from pathlib import Path
import threading
import unittest
from urllib.request import Request, urlopen


class AnalysisTests(unittest.TestCase):
    def test_inventory_mismatch_and_within_artifact_drift_are_rejected(self):
        path = Path(__file__).with_name('analyze.py')
        spec = importlib.util.spec_from_file_location('soak_analyze_inventory', path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        self.assertTrue(hasattr(module, 'matching_inventory'), 'artifact inventory validation missing')
        empty = {'mock': {'inventory_count': 0, 'inventory_bytes': 0, 'inventory_sha256': None}}
        observed = {'mock': {'inventory_count': 36, 'inventory_bytes': 1234,
                             'inventory_sha256': 'a' * 64}}
        changed = {'mock': dict(observed['mock'], inventory_sha256='b' * 64)}
        self.assertEqual(module.matching_inventory([empty, observed], [observed]), observed['mock'])
        for a, b in [([observed], [changed]), ([observed, changed], [observed]),
                     ([empty], [observed]), ([observed, empty], [observed])]:
            with self.assertRaises(ValueError):
                module.matching_inventory(a, b)

    def test_matching_is_by_completed_turn_after_both_warmups(self):
        path = Path(__file__).with_name('analyze.py')
        self.assertTrue(path.exists(), 'matched-turn analyzer is missing')
        spec = importlib.util.spec_from_file_location('soak_analyze', path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        a = [{'elapsed_s': t, 'completed_turns': t, 'rss_kib': 100 + 2*t,
              'pss_kib': 80+t} for t in range(12)]
        b = [{'elapsed_s': 2*t, 'completed_turns': t, 'rss_kib': 100 + t,
              'pss_kib': 80+t} for t in range(9)]
        result = module.compare(a, b, 4, 4)
        self.assertEqual(result['completed_turn_range'], [4, 8])
        self.assertEqual(result['before']['rss_kib_per_turn'], 2)
        self.assertEqual(result['after']['rss_kib_per_turn'], 1)


class DriverTests(unittest.TestCase):
    def test_terminal_must_match_counts_and_response(self):
        path = Path(__file__).with_name('soak.py')
        self.assertTrue(path.exists(), 'driver implementation is missing')
        spec = importlib.util.spec_from_file_location('soak_driver', path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        metrics = {'tool_calls': 25, 'provider_calls': 26, 'inventory_count': 36,
                   'errors': 0, 'lists': [1, 1, 1]}
        module.validate_turn(1, {'full_response': 'SOAK_DONE:1:25'}, metrics)
        for key, value in [('tool_calls', 24), ('provider_calls', 27), ('errors', 1)]:
            with self.assertRaises(ValueError):
                module.validate_turn(1, {'full_response': 'SOAK_DONE:1:25'},
                                     dict(metrics, **{key: value}))


class HarnessTests(unittest.TestCase):
    def setUp(self):
        path = Path(__file__).with_name('mock.py')
        self.assertTrue(path.exists(), 'HTTP mock implementation is missing')
        spec = importlib.util.spec_from_file_location('soak_mock', path)
        self.mock = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.mock)
        self.server = self.mock.make_server('127.0.0.1', 0)
        self.thread = threading.Thread(target=self.server.serve_forever)
        self.thread.start()
        self.base = 'http://127.0.0.1:%s' % self.server.server_port

    def tearDown(self):
        if hasattr(self, 'server'):
            self.server.shutdown()
            self.server.server_close()
            self.thread.join()

    def post(self, path, data):
        with urlopen(Request(self.base + path, json.dumps(data).encode(),
                             {'Content-Type': 'application/json'}), timeout=3) as r:
            body = r.read()
            return json.loads(body) if body else None

    def test_real_http_inventory_and_25_call_turn(self):
        tools = []
        for server in range(3):
            path = '/mcp/%s' % server
            init = self.post(path, {'id': 1, 'method': 'initialize'})
            self.assertEqual(init['result']['capabilities'], {'tools': {}})
            self.assertIsNone(self.post(path, {'method': 'notifications/initialized'}))
            result = self.post(path, {'id': 2, 'method': 'tools/list'})
            self.assertEqual(len(result['result']['tools']), 12)
            for tool in result['result']['tools']:
                self.assertTrue(3000 <= len(json.dumps(tool['inputSchema'])) <= 3300)
                tools.append({'type': 'function', 'function': {
                    'name': 'soak%s__%s' % (server, tool['name']),
                    'parameters': tool['inputSchema']}})
        self.post('/turn', {'turn': 1})
        ids = set()
        for step in range(25):
            result = self.post('/v1/chat/completions', {'tools': tools})
            call = result['choices'][0]['message']['tool_calls'][0]
            ids.add(call['id'])
            name = call['function']['name']
            args = json.loads(call['function']['arguments'])
            server, tool = name.split('__')
            self.post('/mcp/' + server[-1], {'id': step + 3, 'method': 'tools/call',
                      'params': {'name': tool, 'arguments': args}})
        final = self.post('/v1/chat/completions', {'tools': tools})
        self.assertEqual(final['choices'][0]['message']['content'], 'SOAK_DONE:1:25')
        self.assertEqual(len(ids), 25)
        with urlopen(self.base + '/metrics', timeout=3) as r:
            metrics = json.load(r)
        self.assertEqual(metrics['tool_calls'], 25)
        self.assertEqual(metrics['provider_calls'], 26)
        self.assertEqual(metrics['inventory_count'], 36)
        self.assertEqual(metrics['errors'], 0)

    def test_provider_cannot_advance_without_real_tool_execution(self):
        from urllib.error import HTTPError
        self.post('/turn', {'turn': 1})
        tools = [{'function': {'name': n}} for n in self.mock.expected_names()]
        self.post('/v1/chat/completions', {'tools': tools})
        with self.assertRaises(HTTPError) as raised:
            self.post('/v1/chat/completions', {'tools': tools})
        self.assertEqual(raised.exception.code, 400)

    def test_streaming_inventory_fingerprint_is_stable_and_changes_fail(self):
        from urllib.error import HTTPError
        self.post('/turn', {'turn': 1})
        tools = [{'type': 'function', 'function': {'name': n, 'parameters': self.mock.schema()}}
                 for n in self.mock.expected_names()]
        body = {'tools': tools, 'stream': True}
        with urlopen(Request(self.base + '/v1/chat/completions', json.dumps(body).encode(),
                             {'Content-Type': 'application/json'}), timeout=3) as r:
            self.assertEqual(r.headers['Content-Type'], 'text/event-stream')
            events = r.read().decode().split('\n\n')
        first = json.loads(events[0][6:])
        call = first['choices'][0]['delta']['tool_calls'][0]
        self.assertEqual(call['index'], 0)
        self.assertEqual(events[1], 'data: [DONE]')
        self.post('/mcp/0', {'id': 1, 'method': 'tools/call', 'params': {
            'name': 'echo00', 'arguments': json.loads(call['function']['arguments'])}})
        with urlopen(self.base + '/metrics', timeout=3) as r:
            metrics = json.load(r)
        payload = json.dumps(tools, sort_keys=True, separators=(',', ':')).encode()
        self.assertEqual(metrics['inventory_sha256'], hashlib.sha256(payload).hexdigest())
        self.assertEqual(metrics['inventory_bytes'], len(payload))
        tools[0]['function']['parameters']['description'] = 'unexpected drift'
        with self.assertRaises(HTTPError):
            self.post('/v1/chat/completions', {'tools': tools})


if __name__ == '__main__':
    unittest.main()
