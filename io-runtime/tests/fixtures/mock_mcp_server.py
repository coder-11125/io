#!/usr/bin/env python3
"""A tiny MCP server for io's integration tests.

    mock_mcp_server.py stdio
    mock_mcp_server.py http [--sse-replies] [--require-header]
    mock_mcp_server.py sse
    mock_mcp_server.py exit-early        # prints to stderr and exits

HTTP and SSE modes listen on an ephemeral port and print `PORT <n>` first.
"""
import json
import os
import queue
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYGD4DwABBAEAwS2OUAAAAABJRU5ErkJggg=="

TOOLS_PAGE_1 = [
    {
        "name": "echo",
        "description": "Echo the text back",
        "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]},
        "annotations": {"readOnlyHint": True},
    }
]
TOOLS_PAGE_2 = [
    {"name": "add", "description": "Add two numbers",
     "inputSchema": {"type": "object", "properties": {"a": {"type": "number"}, "b": {"type": "number"}}}},
    {"name": "fail", "description": "Always reports an error", "inputSchema": {"type": "object"}},
    {"name": "image", "description": "Return a 1x1 PNG", "inputSchema": {"type": "object"}},
    {"name": "env", "description": "Read an environment variable",
     "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}}}},
    {"name": "slow", "description": "Sleep for a while", "inputSchema": {"type": "object"}},
    {"name": "crash", "description": "Kill the server", "inputSchema": {"type": "object"}},
]


def text(s, error=False):
    return {"content": [{"type": "text", "text": s}], "isError": error}


def call(name, args):
    if name == "echo":
        return text("echo: " + args.get("text", ""))
    if name == "add":
        return text(str(args.get("a", 0) + args.get("b", 0)))
    if name == "fail":
        return text("this tool always fails", error=True)
    if name == "image":
        return {"content": [{"type": "image", "data": PNG, "mimeType": "image/png"}]}
    if name == "env":
        return text(os.environ.get(args.get("name", ""), "<unset>"))
    if name == "slow":
        time.sleep(5)
        return text("finally")
    if name == "crash":
        os._exit(3)
    return None


def handle(msg):
    """Return the response for a JSON-RPC message, or None for notifications."""
    method = msg.get("method")
    mid = msg.get("id")
    if mid is None:
        return None
    if method == "initialize":
        result = {"protocolVersion": "2025-03-26", "capabilities": {"tools": {}},
                  "serverInfo": {"name": "mock", "version": "0.1"}}
    elif method == "tools/list":
        if msg.get("params", {}).get("cursor") == "2":
            result = {"tools": TOOLS_PAGE_2}
        else:
            result = {"tools": TOOLS_PAGE_1, "nextCursor": "2"}
    elif method == "tools/call":
        p = msg.get("params", {})
        r = call(p.get("name"), p.get("arguments", {}))
        if r is None:
            return {"jsonrpc": "2.0", "id": mid, "error": {"code": -32602, "message": "unknown tool"}}
        result = r
    elif method == "ping":
        result = {}
    else:
        return {"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "no such method"}}
    return {"jsonrpc": "2.0", "id": mid, "result": result}


def run_stdio():
    # Stray non-JSON output must be tolerated by the client.
    print("starting up (not json)", flush=True)
    lock = threading.Lock()

    def serve(msg):
        resp = handle(msg)
        if resp is not None:
            with lock:
                print(json.dumps(resp), flush=True)

    # One thread per request, so a slow tool doesn't block the others.
    for line in sys.stdin:
        line = line.strip()
        if line:
            threading.Thread(target=serve, args=(json.loads(line),), daemon=True).start()


def run_http(sse_replies, require_header):
    session = "sess-123"

    class H(BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))))
            if require_header and self.headers.get("X-Test") != "yes":
                self.send_response(401)
                self.end_headers()
                return
            if body.get("method") != "initialize" and self.headers.get("Mcp-Session-Id") != session:
                self.send_response(400)
                self.end_headers()
                self.wfile.write(b"missing session")
                return
            resp = handle(body)
            if resp is None:
                self.send_response(202)
                self.end_headers()
                return
            self.send_response(200)
            if body.get("method") == "initialize":
                self.send_header("Mcp-Session-Id", session)
            if sse_replies:
                payload = (
                    "event: message\ndata: " + json.dumps({"jsonrpc": "2.0", "method": "notifications/progress", "params": {}}) + "\n\n"
                    "event: message\ndata: " + json.dumps(resp) + "\n\n"
                ).encode()
                self.send_header("Content-Type", "text/event-stream")
            else:
                payload = json.dumps(resp).encode()
                self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    srv = ThreadingHTTPServer(("127.0.0.1", 0), H)
    print("PORT", srv.server_address[1], flush=True)
    srv.serve_forever()


def run_sse():
    outbox = queue.Queue()

    class H(BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def do_GET(self):
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            self.wfile.write(b"event: endpoint\ndata: /messages?sid=abc\n\n")
            self.wfile.flush()
            while True:
                msg = outbox.get()
                self.wfile.write(("event: message\ndata: " + json.dumps(msg) + "\n\n").encode())
                self.wfile.flush()

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))))
            resp = handle(body)
            if resp is not None:
                outbox.put(resp)
            self.send_response(202)
            self.send_header("Content-Length", "0")
            self.end_headers()

    srv = ThreadingHTTPServer(("127.0.0.1", 0), H)
    print("PORT", srv.server_address[1], flush=True)
    srv.serve_forever()


if __name__ == "__main__":
    mode = sys.argv[1]
    if mode == "stdio":
        run_stdio()
    elif mode == "http":
        run_http("--sse-replies" in sys.argv, "--require-header" in sys.argv)
    elif mode == "sse":
        run_sse()
    elif mode == "exit-early":
        sys.stderr.write("fatal: missing configuration\n")
        sys.exit(1)
