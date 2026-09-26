#!/usr/bin/env python3
"""Devnet demo dashboard (deploy only): the nodes the nameserver resolved from ENS, the latency map, container
logs, a terminal per container (Dozzle), "add a node at a location" (deploy/scripts/add-node.sh), and a client's
attestation flow: verify a human through the World ID mock (deploy/scripts/human.sh), then measure nodes from that
city's user container and submit the batch to the contract (deploy/scripts/attest.sh).
Endpoints: GET / (page), GET /api/state, GET /api/logs/<container>?tail=200, GET /api/jobs/<id>, and the jobs
(202 with a job id): POST /clients {label, lat, lon} (a human's device; deploy/scripts/add-client.sh), POST /nodes
{label, lat, lon} (a CDN node with a local human), POST /api/worldid {city, index?}, POST /api/attest {city, index?,
nodes?}. Stdlib only."""
import glob
import json
import os
import subprocess
import threading
import urllib.parse
import urllib.request
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

NS_API = os.environ.get("NS_API", "http://127.0.0.1:8055")
DOZZLE = os.environ.get("DOZZLE_URL", "http://localhost:9999")
ROOT = os.path.dirname(os.path.abspath(__file__))
jobs = {}  # id -> {label, status, log}


def ns(path):
    with urllib.request.urlopen(NS_API + path, timeout=5) as r:
        return json.load(r)


def containers():
    out = subprocess.run(
        ["docker", "ps", "-a", "--no-trunc", "--filter", "name=dedan-devnet", "--format", "{{.ID}}\t{{.Names}}\t{{.Status}}"],
        capture_output=True, text=True).stdout
    rows = [dict(zip(("id", "name", "status"), line.split("\t"))) for line in out.splitlines() if line]
    return sorted(rows, key=lambda r: r["name"])


def humans():
    """Every human with a user container: the static city users (verified at devnet start), the local human of each
    added node city, and clients added at runtime (unverified until the World ID step)."""
    out = []
    files = [("city", "deploy/devnet/nodes.json")] + [("city", f) for f in sorted(glob.glob("deploy/devnet/dynamic/*/node.json"))] \
        + [("client", f) for f in sorted(glob.glob("deploy/devnet/clients/*/client.json"))]
    for kind, f in files:
        try:
            with open(f) as fh:
                for n in json.load(fh)["nodes"]:
                    out.append({"city": n["label"], "kind": kind, "attester": n["attester"], "cell": n["cell"], "location": n["location"],
                                "ip": n["ip"], "verified_at_start": kind == "city"})
        except OSError:
            pass
    return out


def run_job(job, cmd):
    p = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, env={**os.environ, "NS_API": NS_API})
    for line in p.stdout:
        job["log"] += line
    job["status"] = "done" if p.wait() == 0 else "failed"


def start_job(kind, label, cmd):
    job_id = uuid.uuid4().hex[:8]
    jobs[job_id] = {"kind": kind, "label": label, "status": "running", "log": "$ " + " ".join(cmd) + "\n"}
    threading.Thread(target=run_job, args=(jobs[job_id], cmd), daemon=True).start()
    return {"job": job_id, "watch": f"/api/jobs/{job_id}", "label": label}


class Handler(BaseHTTPRequestHandler):
    def reply(self, code, body, ctype="application/json"):
        data = body if isinstance(body, bytes) else (json.dumps(body) if ctype == "application/json" else body).encode()
        self.send_response(code)
        self.send_header("content-type", ctype)
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        u = urllib.parse.urlparse(self.path)
        q = urllib.parse.parse_qs(u.query)
        if u.path == "/":
            with open(os.path.join(ROOT, "index.html"), "rb") as f:
                return self.reply(200, f.read(), "text/html; charset=utf-8")
        if u.path == "/api/state":
            state = {"dozzle": DOZZLE, "containers": containers(), "jobs": jobs, "cities": humans()}
            for k in ("nodes", "map", "assignment"):
                try:
                    state[k] = ns("/" + k)
                except Exception as e:  # the nameserver may still be syncing
                    state[k] = {"error": str(e)}
            # H3 cells exceed 2^53: browsers round them, so they travel as strings
            for r in state["map"].get("ranks", []):
                r["cell"] = str(r["cell"])
            for e in state["map"].get("rejected", []):
                e["cell"] = str(e.get("cell", ""))
            return self.reply(200, state)
        if u.path.startswith("/api/logs/"):
            name = u.path.rsplit("/", 1)[1]
            out = subprocess.run(["docker", "logs", "--tail", q.get("tail", ["200"])[0], name], capture_output=True, text=True)
            return self.reply(200, out.stdout + out.stderr, "text/plain; charset=utf-8")
        if u.path.startswith("/api/jobs/"):
            job = jobs.get(u.path.rsplit("/", 1)[1])
            return self.reply(200 if job else 404, job or {"error": "no such job"})
        self.reply(404, {"error": "not found"})

    def do_POST(self):
        try:
            body = json.loads(self.rfile.read(int(self.headers.get("content-length") or 0)) or b"{}")
            if self.path in ("/nodes", "/api/nodes", "/clients", "/api/clients"):
                label, lat, lon = str(body["label"]), float(body["lat"]), float(body["lon"])
                kind = str(body.get("kind") or ("node" if self.path.endswith("nodes") else "client"))
                if kind not in ("client", "node"):
                    return self.reply(400, {"error": "kind must be client or node"})
                if any(j["label"] == label and j["status"] == "running" for j in jobs.values()):
                    return self.reply(409, {"error": f"{label} is already being added"})
                script = "deploy/scripts/add-client.sh" if kind == "client" else "deploy/scripts/add-node.sh"
                return self.reply(202, start_job(f"add-{kind}", label, ["sh", script, label, str(lat), str(lon)]))
            city = str(body["city"])
            index = body.get("index")
            index = "-" if index in (None, "") else str(int(index))
            if self.path == "/api/worldid":
                cmd = ["sh", "deploy/scripts/human.sh", city] + ([] if index == "-" else [index])
                return self.reply(202, start_job("worldid", city, cmd))
            if self.path == "/api/attest":
                nodes = [str(n) for n in body.get("nodes") or []]
                return self.reply(202, start_job("attest", city, ["sh", "deploy/scripts/attest.sh", city, index] + nodes))
        except Exception as e:
            return self.reply(400, {"error": f"bad request: {e}"})
        self.reply(404, {"error": "not found"})

    def log_message(self, *_):
        pass


if __name__ == "__main__":
    port = int(os.environ.get("PORT", "9090"))
    print(f"dashboard on :{port}, nameserver {NS_API}, dozzle {DOZZLE}", flush=True)
    ThreadingHTTPServer(("0.0.0.0", port), Handler).serve_forever()
