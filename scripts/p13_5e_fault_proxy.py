#!/usr/bin/env python3
"""Deterministic test-only HTTP fault proxy for P13.5E.

This module deliberately has no production imports.  A rule applies once to a
matching request and records forwarding and backend completion before a client
response is returned or dropped.
"""
from __future__ import annotations

import argparse
import json
import signal
import threading
from dataclasses import asdict, dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from http.client import HTTPConnection
from urllib.parse import urlsplit


@dataclass
class FaultRule:
    method: str
    path: str
    location: str
    kind: str
    remaining: int = 1


@dataclass
class ProxyRecord:
    request_number: int
    method: str
    path: str
    forwarded: bool
    backend_status: int | None
    response_to_client: str
    fault_location: str | None
    fault_kind: str | None


class FaultProxy:
    def __init__(self, backend: str, rules: list[FaultRule] | None = None, listen_port: int = 0):
        self.backend = backend
        self.rules = rules or []
        self.records: list[ProxyRecord] = []
        self._lock = threading.Lock()
        target = urlsplit(backend)
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self): owner._handle(self)
            def do_POST(self): owner._handle(self)
            def do_PUT(self): owner._handle(self)
            def do_DELETE(self): owner._handle(self)
            def log_message(self, *_args): pass

        self.server = ThreadingHTTPServer(("127.0.0.1", listen_port), Handler)
        self._target = (target.hostname, target.port or 80, target.path.rstrip("/"))
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    @property
    def address(self):
        return f"http://127.0.0.1:{self.server.server_port}"

    def start(self): self.thread.start()
    def close(self): self.server.shutdown(); self.server.server_close(); self.thread.join()

    def _handle(self, request):
        with self._lock:
            number = len(self.records) + 1
            request_path = request.path.split("?", 1)[0]
            matches = lambda pattern: pattern == "*" or (pattern.endswith("*") and request_path.startswith(pattern[:-1])) or pattern == request_path
            rule = next((r for r in self.rules if r.remaining and r.method == request.command and matches(r.path)), None)
            if rule: rule.remaining -= 1
        if rule and rule.location == "before_forward":
            record = ProxyRecord(number, request.command, request.path, False, None, "error", rule.location, rule.kind)
            self.records.append(record)
            request.send_error(503, "deterministic fault")
            return
        body = request.rfile.read(int(request.headers.get("Content-Length", "0")))
        connection = HTTPConnection(self._target[0], self._target[1], timeout=10)
        connection.request(request.command, self._target[2] + request.path, body=body)
        response = connection.getresponse()
        payload = response.read()
        record = ProxyRecord(number, request.command, request.path, True, response.status, "delivered", None, None)
        if rule and rule.location in {"after_commit_before_response", "read_response_drop"}:
            record.response_to_client = "dropped"
            record.fault_location, record.fault_kind = rule.location, rule.kind
            self.records.append(record)
            request.send_error(503, "deterministic dropped response")
            return
        self.records.append(record)
        request.send_response(response.status)
        for key, value in response.getheaders():
            if key.lower() not in {"connection", "transfer-encoding", "content-length"}:
                request.send_header(key, value)
        request.send_header("Content-Length", str(len(payload)))
        request.end_headers(); request.wfile.write(payload)

    def evidence(self):
        return [asdict(record) for record in self.records]


def self_test():
    class BackendHandler(BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200)
            self.send_header("Content-Length", "2")
            self.end_headers()
            self.wfile.write(b"ok")

        def do_PUT(self):
            self.send_response(204)
            self.send_header("Content-Length", "0")
            self.end_headers()

        def log_message(self, *_args): pass

    backend = ThreadingHTTPServer(("127.0.0.1", 0), BackendHandler)
    backend_thread = threading.Thread(target=backend.serve_forever, daemon=True)
    backend_thread.start()
    proxy = FaultProxy(
        f"http://127.0.0.1:{backend.server_port}",
        [
            FaultRule("GET", "/before", "before_forward", "connection_reset"),
            FaultRule("GET", "/drop", "read_response_drop", "response_drop"),
            FaultRule("PUT", "/commit", "after_commit_before_response", "response_drop"),
        ],
    )
    proxy.start()
    try:
        for path in ("/before", "/drop"):
            connection = HTTPConnection("127.0.0.1", proxy.server.server_port, timeout=2)
            connection.request("GET", path)
            assert connection.getresponse().status == 503
            connection.close()
        connection = HTTPConnection("127.0.0.1", proxy.server.server_port, timeout=2)
        connection.request("PUT", "/commit")
        assert connection.getresponse().status == 503
        connection.close()
        records = proxy.evidence()
        assert records[0]["forwarded"] is False
        assert records[0]["fault_location"] == "before_forward"
        assert records[1]["forwarded"] is True
        assert records[1]["backend_status"] == 200
        assert records[1]["response_to_client"] == "dropped"
        assert records[2]["forwarded"] is True
        assert records[2]["backend_status"] == 204
        assert records[2]["fault_location"] == "after_commit_before_response"
    finally:
        proxy.close()
        backend.shutdown()
        backend.server_close()
        backend_thread.join()
    print("P13.5E fault proxy self-test: PASS")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--serve-backend")
    parser.add_argument("--evidence")
    parser.add_argument("--rule", action="append", default=[], help="METHOD PATH LOCATION KIND")
    parser.add_argument("--listen-port", type=int, default=0)
    args = parser.parse_args()
    if args.self_test: self_test()
    elif args.serve_backend:
        rules = []
        for value in args.rule:
            fields = value.split(" ", 3)
            if len(fields) != 4:
                parser.error("--rule must be: METHOD PATH LOCATION KIND")
            rules.append(FaultRule(*fields))
        proxy = FaultProxy(args.serve_backend, rules, args.listen_port)
        proxy.start()
        print(proxy.address, flush=True)
        def shutdown(_signum, _frame):
            if args.evidence:
                with open(args.evidence, "w", encoding="utf-8") as stream:
                    json.dump({"records": proxy.evidence()}, stream, indent=2)
            proxy.close()
            raise SystemExit(0)
        signal.signal(signal.SIGTERM, shutdown)
        try:
            threading.Event().wait()
        except KeyboardInterrupt:
            pass
        finally:
            if args.evidence:
                with open(args.evidence, "w", encoding="utf-8") as stream:
                    json.dump({"records": proxy.evidence()}, stream, indent=2)
            proxy.close()
    else: parser.error("the proxy is test infrastructure; use --self-test or import it")
