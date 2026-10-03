"""External HTTP fixtures. Production binaries own all application behavior."""

import base64
import collections
import http.server
import json
import subprocess
import threading
import time
import uuid


def b64(value):
    return base64.urlsafe_b64encode(value).rstrip(b"=").decode()


class HttpFixture:
    def __init__(self):
        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_GET(self):
                self.dispatch()

            def do_POST(self):
                self.dispatch()

            def dispatch(self):
                try:
                    size = int(self.headers.get("Content-Length", "0"))
                    if not 0 <= size <= 4 * 1024 * 1024:
                        raise ValueError("invalid fixture request size")
                    body = self.rfile.read(size)
                    status, content_type, response = owner.respond(self.command, self.path, body)
                    data = response if isinstance(response, bytes) else json.dumps(response).encode()
                    self.send_response(status)
                    self.send_header("Content-Type", content_type)
                    self.send_header("Content-Length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
                except Exception as error:
                    owner.errors.append(str(error))
                    self.send_error(500, "fixture contract violation")

        self.errors = []
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.url = "http://127.0.0.1:" + str(self.server.server_port)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=3)


class ModelFixture(HttpFixture):
    def __init__(self):
        self.steps = collections.deque()
        self.requests = []
        self.warmups = 0
        self.lock = threading.Lock()
        super().__init__()

    def enqueue(self, expected, text=None, tool=None):
        self.steps.append((expected, text, tool))

    def respond(self, method, path, body):
        if method == "GET" and path == "/models":
            self.warmups += 1
            return 200, "application/json", {"object": "list", "data": [{"id": "acceptance-model", "object": "model"}]}
        if method != "POST" or path != "/chat/completions":
            raise AssertionError("unexpected model route: " + method + " " + path)
        request = json.loads(body)
        with self.lock:
            self.requests.append(request)
            if not self.steps:
                raise AssertionError("model script exhausted")
            expected, text, tool = self.steps.popleft()
        messages = request.get("messages", [])
        if expected not in json.dumps(messages):
            raise AssertionError("model request missing expected conversation marker: " + expected)
        if request.get("model") != "acceptance-model":
            raise AssertionError("wrong configured model")
        message = {"role": "assistant", "content": text}
        reason = "stop"
        if tool:
            message["tool_calls"] = [{"id": "acceptance-call", "type": "function", "function": {
                "name": tool[0], "arguments": json.dumps(tool[1]),
            }}]
            reason = "tool_calls"
        if request.get("stream"):
            chunks = []
            if tool:
                delta = {"role": "assistant", "tool_calls": [dict(message["tool_calls"][0], index=0)]}
                chunks.append({"choices": [{"index": 0, "delta": delta, "finish_reason": None}]})
            else:
                # Multiple wire chunks ensure the real streaming parser is used.
                midpoint = max(1, len(text) // 2)
                for piece in (text[:midpoint], text[midpoint:]):
                    chunks.append({"choices": [{"index": 0, "delta": {"content": piece}, "finish_reason": None}]})
            chunks.append({"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]})
            wire = "".join("data: " + json.dumps(chunk) + "\n\n" for chunk in chunks) + "data: [DONE]\n\n"
            return 200, "text/event-stream", wire.encode()
        return 200, "application/json", {"choices": [{"message": message, "finish_reason": reason}],
                                          "usage": {"prompt_tokens": 10, "completion_tokens": 5}}

    def verify(self):
        if self.errors or self.steps:
            raise AssertionError(f"model fixture: errors={self.errors}, unused steps={len(self.steps)}")


class OidcFixture(HttpFixture):
    def __init__(self, root, secrets):
        self.key = root / "issuer.key"
        subprocess.run(["openssl", "genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048",
                        "-out", str(self.key)], check=True, capture_output=True, timeout=30)
        self.key.chmod(0o600)
        modulus = subprocess.check_output(["openssl", "rsa", "-in", str(self.key), "-noout", "-modulus"], timeout=10)
        self.modulus = b64(bytes.fromhex(modulus.decode().strip().split("=", 1)[1]))
        self.secrets = secrets
        self.calls = collections.Counter()
        super().__init__()

    def token(self, **changes):
        now = int(time.time())
        claims = {"iss": self.url, "aud": "zeroclaw", "sub": "acceptance-user", "client_id": "acceptance-client",
                  "iat": now, "exp": now + 600, "jti": str(uuid.uuid4()), "groups": ["acceptance-users"]}
        claims.update(changes)
        header = {"alg": "RS256", "typ": "at+jwt", "kid": "acceptance-key"}
        signed = (b64(json.dumps(header).encode()) + "." + b64(json.dumps(claims).encode())).encode()
        signature = subprocess.run(["openssl", "dgst", "-sha256", "-sign", str(self.key)], input=signed,
                                   capture_output=True, check=True, timeout=10).stdout
        token = signed.decode() + "." + b64(signature)
        self.secrets.add(token)
        return token

    def respond(self, method, path, body):
        self.calls[path] += 1
        if method == "GET" and path == "/.well-known/openid-configuration":
            response = {"issuer": self.url, "jwks_uri": self.url + "/jwks", "token_endpoint": self.url + "/token"}
        elif method == "GET" and path == "/jwks":
            response = {"keys": [{"kty": "RSA", "alg": "RS256", "use": "sig", "kid": "acceptance-key",
                                  "n": self.modulus, "e": "AQAB"}]}
        elif method == "POST" and path == "/token" and b"grant_type=client_credentials" in body:
            response = {"access_token": self.token(client_id="acceptance-service"), "token_type": "Bearer", "expires_in": 600}
        else:
            raise AssertionError("unexpected issuer route: " + method + " " + path)
        return 200, "application/json", response
