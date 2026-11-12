"""Sequential real-gateway driver; run in Linux with target runtime's PID namespace."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import threading
import time
from urllib.request import Request, urlopen


def http_json(url, body=None):
    request = Request(url, None if body is None else json.dumps(body).encode(),
                      {'Content-Type': 'application/json'})
    with urlopen(request, timeout=5) as response:
        return json.load(response)


def validate_turn(turn, done, metrics):
    if done.get('full_response') != f'SOAK_DONE:{turn}:25':
        raise ValueError('terminal response does not match current completed workload')
    expected = {'tool_calls': turn * 25, 'provider_calls': turn * 26,
                'inventory_count': 36, 'errors': 0}
    if any(metrics.get(k) != v for k, v in expected.items()):
        raise ValueError(f'workload counter mismatch: {metrics}')
    if len(metrics.get('lists', [])) != 3 or not all(metrics['lists']):
        raise ValueError('not all three MCP inventories were discovered')


def process_identity(pid):
    base = Path('/proc') / str(pid)
    stat = (base / 'stat').read_text()
    # The comm field can contain spaces and parentheses.
    start_ticks = stat[stat.rfind(')') + 2:].split()[19]
    return {'pid': pid, 'start_ticks': start_ticks, 'exe': os.readlink(base / 'exe')}


def memory_sample(pid, identity):
    if process_identity(pid) != identity:
        raise ValueError('target process identity changed')
    base = Path('/proc') / str(pid)
    def field(path, name):
        with path.open() as stream:
            for line in stream:
                if line.startswith(name + ':'):
                    return int(line.split()[1])
        raise ValueError(f'missing {name} in {path}')
    return {'rss_kib': field(base / 'status', 'VmRSS'),
            'pss_kib': field(base / 'smaps_rollup', 'Pss')}


class Recorder:
    def __init__(self, path):
        self.stream = open(path, 'x', buffering=1)
        self.lock = threading.Lock()
        self.start = time.monotonic()
        self.completed = 0

    def emit(self, kind, **fields):
        with self.lock:
            row = {'kind': kind, 'monotonic_s': time.monotonic(),
                   'elapsed_s': time.monotonic() - self.start,
                   'completed_turns': self.completed, **fields}
            self.stream.write(json.dumps(row, sort_keys=True) + '\n')


def run(args):
    import websocket  # Debian python3-websocket, or development-only websocket-client.
    identity = process_identity(args.pid)
    if args.pid == os.getpid():
        raise ValueError('target PID must not be the driver')
    with open(f'/proc/{args.pid}/exe', 'rb') as binary:
        digest = hashlib.file_digest(binary, 'sha256').hexdigest()
    recorder = Recorder(args.output)
    stop = threading.Event()
    sampler_failed = threading.Event()
    ws = None
    thread = None
    try:
        recorder.emit('metadata', revision=args.revision, process=identity,
                      binary_sha256=digest, platform=platform.platform(),
                      settings=vars(args), workload={'servers': 3, 'tools': 36,
                      'schema_bytes': 3072, 'calls_per_turn': 25, 'history_messages': 16})
        initial = http_json(args.mock_url + '/metrics')
        if any(initial[k] for k in ('tool_calls', 'provider_calls', 'errors', 'turn')):
            raise ValueError('mock must be fresh for each run')
        def sampler():
            try:
                deadline = time.monotonic()
                while not stop.is_set():
                    metrics = http_json(args.mock_url + '/metrics')
                    recorder.emit('sample', **memory_sample(args.pid, identity), mock=metrics)
                    if metrics['errors']:
                        raise ValueError('mock reported a runtime protocol error')
                    deadline += args.sample_seconds
                    stop.wait(max(0, deadline - time.monotonic()))
            except Exception as exc:
                recorder.emit('error', source='sampler', message=str(exc))
                sampler_failed.set()
        thread = threading.Thread(target=sampler, name='rss-sampler')
        thread.start()
        ws = websocket.create_connection(args.ws_url, timeout=args.turn_timeout,
                                          subprotocols=['zeroclaw.v1'], suppress_origin=True)
        def receive():
            raw = ws.recv()
            if not raw:
                raise ValueError('WebSocket closed before terminal response')
            if len(raw) > 4 * 1024 * 1024:
                raise ValueError('unexpected oversized WebSocket frame')
            event = json.loads(raw)
            if event.get('type') in ('error', 'aborted', 'approval_request'):
                raise ValueError(f'gateway rejected workload: {event}')
            return event
        if receive().get('type') != 'session_start':
            raise ValueError('missing session_start handshake')
        ws.send(json.dumps({'type': 'connect', 'device_name': 'memory-soak'}))
        if receive().get('type') != 'connected':
            raise ValueError('missing connected handshake')
        while time.monotonic() - recorder.start < args.duration_seconds:
            if sampler_failed.is_set():
                raise ValueError('sampler failed; measurement aborted')
            turn = recorder.completed + 1
            http_json(args.mock_url + '/turn', {'turn': turn})
            ws.send(json.dumps({'type': 'message', 'content': f'Synthetic memory soak turn {turn}'}))
            deadline = time.monotonic() + args.turn_timeout
            frame_counts = {}
            while True:
                remaining = deadline - time.monotonic()
                if remaining <= 0 or sampler_failed.is_set():
                    raise TimeoutError('turn deadline exceeded or sampler failed')
                ws.settimeout(remaining)
                event = receive()
                kind = event.get('type', 'unknown')
                # Fixed event taxonomy prevents accumulation of arbitrary keys.
                key = kind if kind in ('tool_call', 'tool_result', 'done') else 'other'
                frame_counts[key] = frame_counts.get(key, 0) + 1
                if kind == 'done':
                    metrics = http_json(args.mock_url + '/metrics')
                    validate_turn(turn, event, metrics)
                    with recorder.lock:
                        recorder.completed = turn
                    recorder.emit('turn', mock=metrics, ws_frames=frame_counts)
                    break
        if not recorder.completed:
            raise ValueError('run completed no turns')
        stop.set()
        thread.join(timeout=12)
        if thread.is_alive():
            raise ValueError('sampler did not stop within its deadline')
        if sampler_failed.is_set():
            raise ValueError('sampler failed')
        recorder.emit('sample', **memory_sample(args.pid, identity),
                      mock=http_json(args.mock_url + '/metrics'))
        recorder.emit('complete')
    except Exception as exc:
        recorder.emit('error', source='driver', message=str(exc))
        try:
            recorder.emit('diagnostic_counters', mock=http_json(args.mock_url + '/metrics'))
        except Exception as counter_error:
            recorder.emit('error', source='counters', message=str(counter_error))
        raise
    finally:
        stop.set()
        if ws is not None:
            ws.close()
        if thread is not None:
            thread.join(timeout=12)
        recorder.stream.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--ws-url', required=True)
    parser.add_argument('--mock-url', required=True)
    parser.add_argument('--pid', type=int, required=True)
    parser.add_argument('--revision', required=True)
    parser.add_argument('--output', required=True)
    parser.add_argument('--duration-seconds', type=float, default=2100)
    parser.add_argument('--warmup-seconds', type=float, default=300)
    parser.add_argument('--sample-seconds', type=float, default=5)
    parser.add_argument('--turn-timeout', type=float, default=120)
    args = parser.parse_args()
    if min(args.duration_seconds, args.sample_seconds, args.turn_timeout) <= 0:
        parser.error('duration, sample interval and timeout must be positive')
    if not 0 <= args.warmup_seconds < args.duration_seconds:
        parser.error('warmup must be nonnegative and shorter than duration')
    run(args)


if __name__ == '__main__':
    main()
