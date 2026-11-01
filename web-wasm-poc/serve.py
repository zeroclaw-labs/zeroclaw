"""Loopback-only PoC assets and read-only gateway proxy, Python standard library."""

import argparse
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parent
GATEWAY_PORT = 42617
ASSETS = {
    "/": ("index.html", "text/html; charset=utf-8"),
    "/index.html": ("index.html", "text/html; charset=utf-8"),
    "/static/poc.css": ("static/poc.css", "text/css; charset=utf-8"),
    "/pkg/skills_poc.js": ("pkg/skills_poc.js", "text/javascript; charset=utf-8"),
    "/pkg/skills_poc_bg.wasm": ("pkg/skills_poc_bg.wasm", "application/wasm"),
}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        # URLs can expose private skill names.
        pass

    def do_GET(self):
        host = self.headers.get("Host", "")
        if host != f"127.0.0.1:{self.server.server_port}":
            self.send_error(403)
            return
        if self.headers.get("Origin") not in (None, f"http://{host}"):
            self.send_error(403)
            return
        path = urlsplit(self.path)
        if path.scheme or path.netloc or not self.path.startswith("/"):
            self.send_error(400)
            return
        if any(part in (".", "..") for part in unquote(path.path).split("/")):
            self.send_error(400)
            return
        if path.path.startswith("/api/"):
            self.proxy_api()
            return
        asset = ASSETS.get(path.path)
        if asset is None and path.path.startswith("/pkg/snippets/") and path.path.endswith(".js"):
            candidate = (ROOT / path.path.lstrip("/")).resolve()
            if candidate.is_relative_to(ROOT.resolve() / "pkg" / "snippets"):
                asset = (candidate, "text/javascript; charset=utf-8")
        if asset is None:
            self.send_error(404)
            return
        try:
            content = (ROOT / asset[0]).read_bytes()
        except OSError:
            self.send_error(404)
            return
        self.reply(200, asset[1], content)

    def reply(self, status, content_type, content):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(content)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.end_headers()
        self.wfile.write(content)

    def proxy_api(self):
        headers = {"Accept": "application/json"}
        authorization = self.headers.get("Authorization")
        if authorization is not None:
            headers["Authorization"] = authorization
        connection = http.client.HTTPConnection("127.0.0.1", GATEWAY_PORT, timeout=10)
        try:
            connection.request("GET", self.path, headers=headers)
            response = connection.getresponse()
            content = response.read()
            self.reply(response.status, response.getheader("Content-Type", "application/json"), content)
        except (OSError, http.client.HTTPException):
            self.send_error(502, "Gateway unavailable")
        finally:
            connection.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=8080)
    args = parser.parse_args()
    with ThreadingHTTPServer(("127.0.0.1", args.port), Handler) as server:
        print(f"PoC: http://127.0.0.1:{server.server_port}/", flush=True)
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            pass


if __name__ == "__main__":
    main()
