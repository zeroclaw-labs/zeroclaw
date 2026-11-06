"""Real HTTP boundary tests for the local serving helper, no gateway required."""

import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from queue import Queue
import tempfile
from threading import Thread
import unittest
from unittest.mock import patch

import serve

ASSET_CASES = (
    ("/", "index.html", "text/html; charset=utf-8", b"<html>Skills</html>"),
    ("/index.html", "index.html", "text/html; charset=utf-8", b"<html>Skills</html>"),
    ("/static/poc.css", "static/poc.css", "text/css; charset=utf-8", b"body { color: red; }"),
    ("/pkg/skills_poc.js", "pkg/skills_poc.js", "text/javascript; charset=utf-8", b"export default 1;"),
    ("/pkg/skills_poc_bg.wasm", "pkg/skills_poc_bg.wasm", "application/wasm", b"\x00asm\x01\x00\x00\x00"),
)


class Gateway(BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        pass

    def do_GET(self):
        self.server.requests.put((self.command, self.path, self.headers.get("Authorization")))
        authorized = self.headers.get("Authorization") == "Bearer test-only-token"
        self.send_response(200 if authorized else 401)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(b'{"agents":["alpha","beta"]}' if authorized else b'{}')

    do_POST = do_GET
    do_PUT = do_GET
    do_PATCH = do_GET
    do_DELETE = do_GET


class ServingTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        for _, relative, _, content in ASSET_CASES:
            destination = self.root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(content)
        self.patch_root = patch.object(serve, "ROOT", self.root)
        self.patch_root.start()
        self.addCleanup(self.patch_root.stop)
        self.gateway = self.start_server(Gateway)
        self.patch_gateway = patch.object(serve, "GATEWAY_PORT", self.gateway.server_port)
        self.patch_gateway.start()
        self.addCleanup(self.patch_gateway.stop)
        self.server = self.start_server(serve.Handler)

    def start_server(self, handler):
        server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
        server.requests = Queue()
        worker = Thread(target=server.serve_forever, daemon=True)
        worker.start()
        self.addCleanup(server.server_close)
        self.addCleanup(worker.join)
        self.addCleanup(server.shutdown)
        return server

    def request(self, path="/", headers=None, method="GET"):
        connection = http.client.HTTPConnection("127.0.0.1", self.server.server_port)
        self.addCleanup(connection.close)
        connection.request(method, path, headers=headers or {})
        response = connection.getresponse()
        return response.status, response.getheader("Content-Type"), response.read()

    def test_assets_have_expected_types(self):
        for path, _, content_type, expected_body in ASSET_CASES:
            with self.subTest(path=path):
                status, actual_type, body = self.request(path)
                self.assertEqual(status, 200)
                self.assertEqual(actual_type, content_type)
                self.assertEqual(body, expected_body)

    def test_missing_asset_returns_404(self):
        (self.root / "pkg/skills_poc_bg.wasm").unlink()
        self.assertEqual(self.request("/pkg/skills_poc_bg.wasm")[0], 404)

    def test_generated_snippet_javascript_is_served(self):
        snippet = self.root / "pkg/snippets/generated/inline0.js"
        snippet.parent.mkdir(parents=True)
        snippet.write_text("export const example = 1;")
        status, content_type, body = self.request("/pkg/snippets/generated/inline0.js")
        self.assertEqual((status, content_type), (200, "text/javascript; charset=utf-8"))
        self.assertIn(b"export const", body)
        self.assertEqual(self.request("/pkg/snippets/generated/secret.txt")[0], 404)

    def test_snippet_symlinks_cannot_escape_generated_directory(self):
        snippet = self.root / "pkg/snippets/escape.js"
        snippet.parent.mkdir(parents=True)
        snippet.symlink_to(self.root / "index.html")
        self.assertEqual(self.request("/pkg/snippets/escape.js")[0], 404)

    def test_bearer_authentication_is_preserved(self):
        status, content_type, body = self.request(
            "/api/config/agent-options", {"Authorization": "Bearer test-only-token"}
        )
        self.assertEqual((status, content_type), (200, "application/json"))
        self.assertEqual(body, b'{"agents":["alpha","beta"]}')
        self.assertEqual(self.gateway.requests.get_nowait(),
                         ("GET", "/api/config/agent-options", "Bearer test-only-token"))

    def test_proxy_preserves_encoded_path_and_query(self):
        path = "/api/skills/bundles/example/skills/a%20b?view=raw&name=a%2Fb"
        self.assertEqual(self.request(path, {"Authorization": "Bearer test-only-token"})[0], 200)
        self.assertEqual(self.gateway.requests.get_nowait(),
                         ("GET", path, "Bearer test-only-token"))

    def test_missing_and_invalid_tokens_remain_unauthorized(self):
        for headers in ({}, {"Authorization": "Bearer invalid"}):
            self.assertEqual(self.request("/api/config/agent-options", headers)[0], 401)
            self.assertEqual(self.gateway.requests.get_nowait(),
                             ("GET", "/api/config/agent-options", headers.get("Authorization")))

    def test_unknown_paths_and_source_files_are_not_served(self):
        for path in ("/Cargo.lock", "/serve.py", "/pkg/", "/unknown"):
            self.assertEqual(self.request(path)[0], 404)

    def test_traversal_and_absolute_requests_are_rejected(self):
        for path in ("/../Cargo.lock", "/%2e%2e/Cargo.lock", "http://example.com/api/test"):
            headers = {"Host": f"127.0.0.1:{self.server.server_port}"}
            self.assertEqual(self.request(path, headers)[0], 400)

    def test_foreign_host_and_origin_are_rejected(self):
        for headers in ({"Host": "example.com"}, {"Origin": "http://example.com"}):
            self.assertEqual(self.request(headers=headers)[0], 403)

    def test_mutating_requests_are_not_supported(self):
        for method in ("POST", "PUT", "PATCH", "DELETE"):
            with self.subTest(method=method):
                self.assertEqual(self.request("/api/test", method=method)[0], 501)
                self.assertTrue(self.gateway.requests.empty())

    def test_upstream_failure_returns_502(self):
        with patch.object(serve, "GATEWAY_PORT", 0):
            self.assertEqual(self.request("/api/config/agent-options")[0], 502)


if __name__ == "__main__":
    unittest.main()
