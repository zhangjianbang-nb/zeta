#!/usr/bin/env python3
"""Mock OpenAI SSE server: round1 bash tool call, round2 submit_acceptance, round3 final text."""
import json
from http.server import BaseHTTPRequestHandler, HTTPServer

CALLS = {"n": 0}


class H(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        n = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(n))
        CALLS["n"] += 1
        assert body.get("stream") is True, "stream must be true"
        assert body.get("model"), "model required"
        tools = body.get("tools")
        assert isinstance(tools, list) and len(tools) > 0, "tools required"
        if CALLS["n"] == 1:
            tc = {"id": "c1", "type": "function",
                  "function": {"name": "bash", "arguments": json.dumps({"command": "echo smoke-ok"})}}
            delta = {"role": "assistant", "content": None, "tool_calls": [tc]}
            finish = "tool_calls"
            usage = None
        elif CALLS["n"] == 2:
            acc = {"task": "smoke", "criteria": ["echo works"], "verify": ["true"]}
            tc = {"id": "c2", "type": "function",
                  "function": {"name": "submit_acceptance", "arguments": json.dumps(acc)}}
            delta = {"role": "assistant", "content": None, "tool_calls": [tc]}
            finish = "tool_calls"
            usage = None
        else:
            delta = {"role": "assistant", "content": "smoke done"}
            finish = "stop"
            usage = {"prompt_tokens": 100, "completion_tokens": 10, "prompt_cache_hit_tokens": 50}

        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        chunk = {"id": "x", "object": "chat.completion.chunk",
                 "choices": [{"index": 0, "delta": delta, "finish_reason": None}]}
        self.w.write(("data: " + json.dumps(chunk) + "\n\n").encode())
        chunk2 = {"id": "x", "object": "chat.completion.chunk",
                  "choices": [{"index": 0, "delta": {}, "finish_reason": finish}]}
        self.w.write(("data: " + json.dumps(chunk2) + "\n\n").encode())
        if usage:
            u = {"id": "x", "choices": [], "usage": usage}
            self.w.write(("data: " + json.dumps(u) + "\n\n").encode())
        self.w.write(b"data: [DONE]\n\n")
        self.w.flush()


if __name__ == "__main__":
    HTTPServer(("127.0.0.1", 18923), H).serve_forever()
