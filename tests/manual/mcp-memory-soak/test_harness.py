import importlib.util
import json
import hashlib
import socket
from concurrent.futures import ThreadPoolExecutor
from contextlib import ExitStack
from queue import Queue
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

    def test_accepted_idle_connection_does_not_starve_metrics(self):
        accepted = threading.Event()
        original = self.server.RequestHandlerClass

        class ObservedHandler(original):
            def setup(self):
                super().setup()
                accepted.set()

        self.server.RequestHandlerClass = ObservedHandler
        # Suppress only the expected closed-client traceback on the unfixed server.
        server_errors = []
        self.server.handle_error = lambda *_: server_errors.append('handler error')
        with socket.create_connection(self.server.server_address, timeout=1) as idle:
            self.assertTrue(accepted.wait(2), 'idle socket was not accepted')
            try:
                with urlopen(self.base + '/metrics', timeout=1) as response:
                    self.assertEqual(json.load(response)['errors'], 0)
            except (TimeoutError, socket.timeout):
                self.fail('accepted idle TCP connection starved the metrics endpoint')
        self.assertEqual(server_errors, [])

    def test_connection_cap_rejects_overload_without_spawning_more_handlers(self):
        from urllib.error import HTTPError
        accepted = Queue(maxsize=self.mock.MAX_CONNECTIONS)
        finished = threading.Event()
        counts = {'finished': 0, 'active': 0, 'peak': 0}
        lock = threading.Lock()
        original = self.server.RequestHandlerClass
        original_process = self.server.process_request_thread

        def observed_process(request, client_address):
            original_process(request, client_address)
            # BoundedHTTPServer has released its semaphore before returning.
            with lock:
                counts['finished'] += 1
                if counts['finished'] == self.mock.MAX_CONNECTIONS:
                    finished.set()

        self.server.process_request_thread = observed_process

        class ObservedHandler(original):
            def setup(self):
                super().setup()
                with lock:
                    counts['active'] += 1
                    counts['peak'] = max(counts['peak'], counts['active'])
                accepted.put_nowait(True)

            def finish(self):
                try:
                    super().finish()
                finally:
                    with lock:
                        counts['active'] -= 1

        self.server.RequestHandlerClass = ObservedHandler
        with ExitStack() as stack:
            for _ in range(self.mock.MAX_CONNECTIONS):
                stack.enter_context(socket.create_connection(self.server.server_address, timeout=1))
                self.assertTrue(accepted.get(timeout=2))
            with self.assertRaises(HTTPError) as raised:
                urlopen(self.base + '/metrics', timeout=1)
            self.assertEqual(raised.exception.code, 503)
            self.assertEqual(json.load(raised.exception),
                             {'error': 'fixture connection limit exceeded'})
        self.assertTrue(finished.wait(2), 'idle request threads failed to release slots after close')
        self.server.RequestHandlerClass = original
        self.server.process_request_thread = original_process
        with urlopen(self.base + '/metrics', timeout=2) as response:
            metrics = json.load(response)
        self.assertEqual(metrics['errors'], 1)
        self.assertEqual(metrics['tool_calls'], 0)
        self.assertEqual(counts['peak'], self.mock.MAX_CONNECTIONS)

    def test_incomplete_body_does_not_hold_state_lock(self):
        reading_body = threading.Event()
        original = self.server.RequestHandlerClass

        class ObservedHandler(original):
            def do_POST(self):
                reading_body.set()
                super().do_POST()

        self.server.RequestHandlerClass = ObservedHandler
        with socket.create_connection(self.server.server_address, timeout=2) as slow:
            slow.sendall(b'POST /turn HTTP/1.0\r\nContent-Length: 11\r\n\r\n')
            self.assertTrue(reading_body.wait(2))
            with urlopen(self.base + '/metrics', timeout=1) as response:
                self.assertEqual(json.load(response)['errors'], 0)
            slow.sendall(b'{"turn": 1}')
            self.assertIn(b'200 OK', slow.recv(4096))

    def test_concurrent_duplicate_calls_execute_once_and_snapshot_is_detached(self):
        from urllib.error import HTTPError
        self.post('/turn', {'turn': 1})
        tools = [{'function': {'name': n}} for n in self.mock.expected_names()]
        call = self.post('/v1/chat/completions', {'tools': tools})['choices'][0]['message']['tool_calls'][0]
        request = {'id': 3, 'method': 'tools/call', 'params': {
            'name': 'echo00', 'arguments': json.loads(call['function']['arguments'])}}
        barrier = threading.Barrier(2)

        def execute():
            barrier.wait(timeout=2)
            try:
                self.post('/mcp/0', request)
                return 200
            except HTTPError as exc:
                return exc.code

        with ThreadPoolExecutor(max_workers=2) as executor:
            outcomes = list(executor.map(lambda _: execute(), range(2)))
        self.assertEqual(sorted(outcomes), [200, 400])
        with urlopen(self.base + '/metrics', timeout=2) as response:
            metrics = json.load(response)
        self.assertEqual(metrics['tool_calls'], 1)
        self.assertEqual(metrics['provider_calls'], 1)
        self.assertEqual(metrics['errors'], 1)
        snapshot = self.server.state.snapshot()
        snapshot['per_server_calls'][0] = -1
        self.assertEqual(self.server.state.snapshot()['per_server_calls'], [1, 0, 0])

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
