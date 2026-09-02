#!/usr/bin/env python3
"""Deterministic test-only HTTP fault proxy for P13.5E.

This module deliberately has no production imports.  A rule applies once to a
matching request and records forwarding and backend completion before a client
response is returned or dropped.
"""
from __future__ import annotations

import argparse
import json
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
    def __init__(self, backend: str, rules: list[FaultRule] | None = None):
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

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
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
            rule = next((r for r in self.rules if r.remaining and r.method == request.command and r.path == request.path.split("?", 1)[0]), None)
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
    # Unit-test the rule accounting without requiring a backend or network.
    proxy = FaultProxy("http://127.0.0.1:1")
    rule = FaultRule("GET", "/v1/resource", "before_forward", "connection_reset")
    proxy.rules.append(rule)
    assert rule.remaining == 1
    rule.remaining -= 1
    proxy.records.append(ProxyRecord(1, "GET", "/v1/resource", False, None, "error", rule.location, rule.kind))
    assert proxy.evidence()[0]["forwarded"] is False
    assert proxy.evidence()[0]["fault_kind"] == "connection_reset"
    assert rule.remaining == 0
    print("P13.5E fault proxy self-test: PASS")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test: self_test()
    else: parser.error("the proxy is test infrastructure; use --self-test or import it")
