"""Bounded synthetic MCP/OpenAI fixture. Never connect this to public networks."""
import argparse
import copy
import hashlib
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import re
from socketserver import ThreadingMixIn
import threading

CALLS = 25
SCHEMA_BYTES = 3072
MAX_CONNECTIONS = 8


def expected_names():
    return [f'soak{s}__echo{t:02}' for s in range(3) for t in range(12)]


def schema():
    value = {'type': 'object', 'properties': {
        'turn': {'type': 'integer'}, 'step': {'type': 'integer'},
        'nonce': {'type': 'string', 'description': ''}},
        'required': ['turn', 'step', 'nonce'], 'additionalProperties': False}
    value['properties']['nonce']['description'] = 'x' * (SCHEMA_BYTES - len(json.dumps(value)))
    return value


class State:
    def __init__(self):
        self.lock = threading.Lock()
        self.metrics = {'tool_calls': 0, 'provider_calls': 0, 'errors': 0,
                        'inventory_count': 0, 'lists': [0, 0, 0],
                        'inventory_sha256': None, 'inventory_bytes': 0,
                        'per_server_calls': [0, 0, 0], 'turn': 0}
        self.step = 0
        self.pending = None
        self.finished = True

    def dispatch(self, path, body):
        with self.lock:
            return self._dispatch(path, body)

    def snapshot(self):
        with self.lock:
            return copy.deepcopy(self.metrics)

    def record_error(self):
        with self.lock:
            self.metrics['errors'] += 1

    def _dispatch(self, path, body):
        if path == '/turn':
            turn = body['turn']
            if not self.finished or turn != self.metrics['turn'] + 1:
                raise ValueError('turn must be sequential and previous turn complete')
            self.metrics['turn'] = turn
            self.step, self.pending, self.finished = 0, None, False
            return {'turn': turn}
        if path == '/v1/chat/completions':
            names = {tool['function']['name'] for tool in body.get('tools', [])}
            if not set(expected_names()).issubset(names):
                raise ValueError('provider did not receive all 36 MCP tools')
            inventory = sorted((tool for tool in body['tools']
                                if tool['function']['name'] in expected_names()),
                               key=lambda tool: tool['function']['name'])
            if len(inventory) != 36:
                raise ValueError('duplicate MCP tool definitions')
            payload = json.dumps(inventory, sort_keys=True, separators=(',', ':')).encode()
            digest = hashlib.sha256(payload).hexdigest()
            if self.metrics['inventory_sha256'] not in (None, digest):
                raise ValueError('MCP tool inventory changed during run')
            self.metrics['inventory_sha256'] = digest
            self.metrics['inventory_bytes'] = len(payload)
            self.metrics['inventory_count'] = 36
            if self.finished or self.pending is not None:
                raise ValueError('unexpected provider request before tool execution or turn start')
            self.metrics['provider_calls'] += 1
            turn = self.metrics['turn']
            if self.step == CALLS:
                self.finished = True
                message = {'role': 'assistant', 'content': f'SOAK_DONE:{turn}:{CALLS}'}
                finish = 'stop'
            else:
                index = ((turn - 1) * CALLS + self.step) % 36
                name = expected_names()[index]
                args = {'turn': turn, 'step': self.step, 'nonce': f'{turn}:{self.step}'}
                self.pending = (index // 12, name.split('__')[1], args)
                message = {'role': 'assistant', 'content': None, 'tool_calls': [{
                    'id': f'call_{turn}_{self.step}', 'type': 'function',
                    'function': {'name': name, 'arguments': json.dumps(args)}}]}
                finish = 'tool_calls'
            return {'id': f'chat_{turn}_{self.step}', 'object': 'chat.completion',
                    'model': 'soak', 'choices': [{'index': 0, 'message': message,
                    'finish_reason': finish}],
                    'usage': {'prompt_tokens': 100, 'completion_tokens': 10, 'total_tokens': 110}}
        match = re.fullmatch(r'/mcp/([012])', path)
        if not match:
            raise ValueError('unknown fixture endpoint')
        server = int(match[1])
        method = body.get('method')
        if method == 'initialize':
            result = {'protocolVersion': body.get('params', {}).get('protocolVersion', '2024-11-05'),
                      'capabilities': {'tools': {}},
                      'serverInfo': {'name': f'soak{server}', 'version': '1'}}
        elif method == 'notifications/initialized':
            return None
        elif method == 'tools/list':
            self.metrics['lists'][server] += 1
            result = {'tools': [{'name': f'echo{t:02}', 'description': 'Synthetic harmless echo',
                                 'inputSchema': schema()} for t in range(12)]}
        elif method == 'tools/call':
            params = body['params']
            if self.pending != (server, params['name'], params['arguments']):
                raise ValueError('unexpected or duplicate MCP execution')
            self.pending = None
            self.step += 1
            self.metrics['tool_calls'] += 1
            self.metrics['per_server_calls'][server] += 1
            result = {'content': [{'type': 'text', 'text': json.dumps(params['arguments'])}],
                      'isError': False}
        else:
            raise ValueError('unsupported MCP method')
        return {'jsonrpc': '2.0', 'id': body.get('id'), 'result': result}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def setup(self):
        super().setup()
        self.connection.settimeout(10)

    def reply(self, status, body, content_type='application/json'):
        data = body if isinstance(body, bytes) else json.dumps(body).encode()
        self.send_response(status)
        self.send_header('Content-Type', content_type)
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path == '/metrics':
            self.reply(200, self.server.state.snapshot())
        else:
            self.reply(404, {'error': 'unknown endpoint'})

    def do_POST(self):
        try:
            length = int(self.headers.get('Content-Length', '0'))
            if not 0 < length <= 4 * 1024 * 1024:
                raise ValueError('request size outside fixture bounds')
            body = json.loads(self.rfile.read(length))
            result = self.server.state.dispatch(self.path, body)
            if result is None:
                self.reply(202, b'')
            elif self.path == '/v1/chat/completions' and body.get('stream'):
                choice = result['choices'][0]
                delta = dict(choice['message'])
                for i, call in enumerate(delta.get('tool_calls', [])):
                    call['index'] = i
                chunk = {'id': result['id'], 'choices': [{'index': 0, 'delta': delta,
                         'finish_reason': choice['finish_reason']}], 'usage': result['usage']}
                self.reply(200, ('data: ' + json.dumps(chunk) + '\n\ndata: [DONE]\n\n').encode(),
                           'text/event-stream')
            else:
                self.reply(200, result)
        except (ValueError, KeyError, TypeError) as exc:
            self.server.state.record_error()
            self.reply(400, {'error': str(exc)})


class BoundedHTTPServer(ThreadingMixIn, HTTPServer):
    # Acquire before spawning: ThreadingMixIn alone permits unbounded threads.
    daemon_threads = False
    block_on_close = True

    def __init__(self, address, handler):
        self.slots = threading.BoundedSemaphore(MAX_CONNECTIONS)
        self.state = State()
        super().__init__(address, handler)

    def process_request(self, request, client_address):
        if not self.slots.acquire(blocking=False):
            self.state.record_error()
            try:
                # Never let overload handling monopolize the accept loop either.
                request.settimeout(0.1)
                body = b'{"error":"fixture connection limit exceeded"}'
                request.sendall(b'HTTP/1.0 503 Service Unavailable\r\n'
                                b'Content-Type: application/json\r\n'
                                b'Connection: close\r\nContent-Length: '
                                + str(len(body)).encode() + b'\r\n\r\n' + body)
            except OSError:
                pass  # The client may already have disconnected; errors stays nonzero.
            finally:
                self.shutdown_request(request)
            return
        try:
            super().process_request(request, client_address)
        except BaseException:
            self.slots.release()
            raise

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            self.slots.release()


def make_server(host, port):
    return BoundedHTTPServer((host, port), Handler)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--host', default='127.0.0.1')
    parser.add_argument('--port', type=int, default=8080)
    args = parser.parse_args()
    with make_server(args.host, args.port) as server:
        server.serve_forever()
