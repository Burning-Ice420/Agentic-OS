"""Mesh hub and live dashboard for the hive cluster.

The hub is the wire, not the brain. It connects to every instance's COM2 socket
and forwards lines:
  * HMSG|jobs|<id>|<worker>|...   -> only that worker
  * HMSG|results|... and load|... -> only the coordinator
  * anything else                 -> every other instance (plain mesh broadcast)
All placement decisions are made by the coordinator's kernel. The hub also
measures each VM's host CPU use (psutil, per QEMU process) and serves a live
dashboard at http://127.0.0.1:8765 .

Used by experiments/cluster_bench.py, or standalone for a demo:
  python services/cluster/hub.py coord=4600 w1=4601 w2=4602 ...   (pids optional: w1=4601:1234)
"""
from __future__ import annotations

import json
import os
import socket
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

try:
    import psutil
except ImportError:  # pragma: no cover
    psutil = None

HERE = os.path.dirname(os.path.abspath(__file__))


class Hub:
    def __init__(self, ports: dict[str, int], pids: dict[str, int] | None = None, coord: str = "coord"):
        self.coord = coord
        self.socks: dict[str, socket.socket] = {}
        self.lock = threading.Lock()
        self.t0 = time.perf_counter()
        self.stop = False
        self.events: list[dict] = []          # every cluster message with a host timestamp
        self.cpu: list[dict] = []             # {t, vm: percent, ..., host: percent}
        self.load: dict[str, dict] = {}       # latest heartbeat per worker
        self.done: dict[str, int] = {}
        self.assigned: dict[str, int] = {}
        self.inflight: dict[str, int] = {}
        self.notes: list[dict] = []
        self.pids = dict(pids or {})
        self.procs = {}
        for name, port in ports.items():
            for _ in range(50):
                try:
                    self.socks[name] = socket.create_connection(("127.0.0.1", port), timeout=5)
                    break
                except OSError:
                    time.sleep(0.2)
        for name, s in self.socks.items():
            threading.Thread(target=self._pump, args=(name, s), daemon=True).start()
        threading.Thread(target=self._sample_cpu, daemon=True).start()

    def now(self) -> float:
        return time.perf_counter() - self.t0

    # ── wire ──
    def _send(self, name: str, line: bytes):
        s = self.socks.get(name)
        if s is None:
            return
        try:
            s.sendall(line + b"\n")
        except OSError:
            pass

    def _pump(self, name: str, s: socket.socket):
        s.settimeout(0.5)
        buf = b""
        while not self.stop:
            try:
                d = s.recv(4096)
            except socket.timeout:
                continue
            except OSError:
                return
            if not d:
                return
            buf += d
            while b"\n" in buf:
                ln, buf = buf.split(b"\n", 1)
                ln = ln.strip()
                if ln.startswith(b"HMSG|"):
                    self._route(name, ln)

    def _route(self, src: str, ln: bytes):
        parts = ln.decode(errors="replace").split("|")
        t = self.now()
        node = parts[1] if len(parts) > 1 else ""
        with self.lock:
            if node == "jobs" and len(parts) >= 5:
                w = parts[3]
                self.events.append({"t": t, "type": "assign", "job": parts[2], "worker": w, "spec": parts[4]})
                self.assigned[w] = self.assigned.get(w, 0) + 1
                self.inflight[w] = self.inflight.get(w, 0) + 1
                dst = [w]
            elif node == "results" and len(parts) >= 6:
                w = parts[3]
                self.events.append({"t": t, "type": "result", "job": parts[2], "worker": w,
                                    "result": parts[4], "ms": _int(parts[5])})
                self.done[w] = self.done.get(w, 0) + 1
                self.inflight[w] = max(0, self.inflight.get(w, 0) - 1)
                dst = [self.coord]
            elif node == "load" and len(parts) >= 6:
                self.load[parts[2]] = {"t": t, "queued": _int(parts[3]), "done": _int(parts[4]),
                                       "busy": _int(parts[5]) / 10.0,
                                       "cores": _int(parts[6]) if len(parts) > 6 else 1}
                dst = [self.coord]
            else:
                dst = [n for n in self.socks if n != src]
        for d in dst:
            self._send(d, ln)

    # ── measurement ──
    def _sample_cpu(self):
        if psutil is None:
            return
        psutil.cpu_percent(None)
        while not self.stop:
            row = {"t": self.now()}
            for name, pid in list(self.pids.items()):
                p = self.procs.get(name)
                if p is None:
                    try:
                        p = psutil.Process(pid)
                        p.cpu_percent(None)
                        self.procs[name] = p
                    except Exception:  # noqa: BLE001
                        continue
                try:
                    # percent of ONE core (QEMU runs one vCPU per VM here)
                    row[name] = p.cpu_percent(None)
                except Exception:  # noqa: BLE001
                    row[name] = 0.0
            row["host"] = psutil.cpu_percent(None)
            with self.lock:
                self.cpu.append(row)
            time.sleep(0.5)

    def note(self, text: str):
        with self.lock:
            self.notes.append({"t": self.now(), "text": text})

    def snapshot(self) -> dict:
        with self.lock:
            return {"t": self.now(), "events": list(self.events), "cpu": list(self.cpu),
                    "load": dict(self.load), "done": dict(self.done), "assigned": dict(self.assigned),
                    "inflight": dict(self.inflight), "notes": list(self.notes),
                    "vms": sorted(self.socks.keys()), "cores": psutil.cpu_count() if psutil else None}

    def close(self):
        self.stop = True
        for s in self.socks.values():
            try:
                s.close()
            except OSError:
                pass


def _int(s: str) -> int:
    try:
        return int(s)
    except ValueError:
        return 0


# ── live dashboard ──
def serve(hub: Hub, port: int = 8765):
    page = open(os.path.join(HERE, "dashboard.html"), encoding="utf-8").read()

    class H(BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def do_GET(self):
            if self.path.startswith("/data"):
                body = json.dumps(hub.snapshot()).encode()
                ctype = "application/json"
            else:
                body = page.encode()
                ctype = "text/html; charset=utf-8"
            self.send_response(200)
            self.send_header("Content-Type", ctype)
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(body)

    srv = ThreadingHTTPServer(("127.0.0.1", port), H)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv


if __name__ == "__main__":
    ports, pids = {}, {}
    for a in sys.argv[1:]:
        name, v = a.split("=", 1)
        if ":" in v:
            p, pid = v.split(":", 1)
            ports[name], pids[name] = int(p), int(pid)
        else:
            ports[name] = int(v)
    hub = Hub(ports, pids)
    serve(hub)
    print("hub up; dashboard at http://127.0.0.1:8765  (Ctrl+C to stop)")
    try:
        while True:
            time.sleep(1)
    except KeyboardInterrupt:
        hub.close()
