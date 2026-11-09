#!/usr/bin/env python3
"""A stand-in for World's Developer Portal verify endpoint (POST /api/v4/verify/<rp_id>), used only by
deploy/scripts/worldid-check.sh: it answers success for any well-formed World ID 4.0 result, echoing its nullifier,
so the rest of the path (the control plane's checks and attestation, the gate, the registry) runs for real without
a World App. The real zero-knowledge verification needs a World ID app (WORLD-ID.md). Stdlib only."""
import json
import os
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Stub(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("content-length") or 0)) or b"{}")
        ok = self.path.startswith("/api/v4/verify/") and body.get("protocol_version") == "4.0" and body.get("responses")
        out = {"success": True, "action": body.get("action"), "nullifier": body["responses"][0].get("nullifier"),
               "environment": body.get("environment"), "results": [{"identifier": r.get("identifier"), "success": True}
                                                                     for r in body["responses"]]} if ok else \
            {"code": "invalid_request", "detail": "not a World ID 4.0 result"}
        data = json.dumps(out).encode()
        self.send_response(200 if ok else 400)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *_):
        pass


if __name__ == "__main__":
    threading.Timer(600, os._exit, [0]).start()  # a check lasts a minute; never outlive it for long
    ThreadingHTTPServer(("0.0.0.0", int(sys.argv[1] if len(sys.argv) > 1 else "9095")), Stub).serve_forever()
