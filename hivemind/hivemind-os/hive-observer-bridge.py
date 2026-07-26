#!/usr/bin/env python3
"""
HiveMind serial -> :8080 observer bridge.

The observer polls http://localhost:8080/hive/snapshot. The bare-metal guest has
no network stack, only serial, so nothing fed that endpoint from a real guest.
This bridge closes that gap: it taps the guest's serial lines and serves the live
graph in exactly the schema the observer expects.

It taps two guest ports (QEMU exposes each as a TCP server; we connect as client):
  COM3 (LLM, default :4455)  LLMREQ/LLMRSP  -> it ALSO acts as the accelerator
                                                (calls Ollama, replies LLMRSP) and
                                                records the request state + decision.
  COM2 (mesh, default :4444) HMSG|mem|key|val -> memory updates from agent broadcasts.

So one process is both the AI accelerator AND the observer feed. Run the observer
against localhost:8080 and it will render the guest's live AI-agent activity.

Run:  python hive-observer-bridge.py            (com2=4444, com3=4455, http=8080)
      python hive-observer-bridge.py --model llama3.2:3b
"""
import argparse, http.server, importlib.util, json, os, socket, sys, threading, time

HERE = os.path.dirname(os.path.abspath(__file__))


def load_bridge():
    spec = importlib.util.spec_from_file_location("llmbridge", os.path.join(HERE, "hive-llm-bridge.py"))
    m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
    return m


class Graph:
    def __init__(self):
        self.lock = threading.Lock()
        self.memories = {}      # name -> {id,name,blobs{key:blob},edges,subs}
        self.events = []
        self.llm_calls = 0
        self.recent_actions = []

    def _coerce(self, v):
        v = v.strip()
        try: return int(v)
        except ValueError:
            try: return float(v)
            except ValueError: return v

    def _upsert(self, name):
        if name not in self.memories:
            self.memories[name] = {"id": str(len(self.memories) + 1), "name": name,
                                   "blobs": {}, "edges": [], "subs": []}
        return self.memories[name]

    def set_blob(self, mem, key, value):
        with self.lock:
            m = self._upsert(mem)
            m["blobs"][key] = {"id": f"{m['id']}-{key}", "key": key,
                               "value": self._coerce(value),
                               "modified_at": int(time.time() * 1000), "read_refs": []}

    def event(self, etype, desc):
        with self.lock:
            self.events.append({"timestamp": int(time.time() * 1000),
                                "event_type": etype, "description": desc})
            self.events = self.events[-60:]

    def propagate(self, from_name, to_name):
        """Record that a value crossed the mesh from one instance to another."""
        with self.lock:
            f = self._upsert(from_name); t = self._upsert(to_name)
            e = {"from_id": f["id"], "to_id": t["id"], "edge_type": "Mirror"}
            if e not in f["edges"]:
                f["edges"].append(e)

    def llm(self, key, value):
        with self.lock:
            self.llm_calls += 1
            self.recent_actions.append(f"{key}={value}")
            self.recent_actions = self.recent_actions[-8:]

    def snapshot(self):
        with self.lock:
            mems, total_blobs = [], 0
            for m in self.memories.values():
                blobs = list(m["blobs"].values()); total_blobs += len(blobs)
                mems.append({"id": m["id"], "name": m["name"], "blobs": blobs,
                             "edges": m["edges"], "subscriptions": m["subs"]})
            agents = [{"id": "ai", "name": "ai-accelerator", "role": "deliberation",
                       "home_memory_id": "1", "status": "active" if self.llm_calls else "idle",
                       "last_actions": self.recent_actions[-5:]}]
            now = time.time() * 1000
            sps = len([e for e in self.events if now - e["timestamp"] < 5000]) / 5.0
            stats = {"total_memories": len(mems), "total_blobs": total_blobs,
                     "total_agents": len(agents), "signals_per_second": round(sps, 2),
                     "llm_calls_total": self.llm_calls, "llm_calls_openai": 0,
                     "llm_calls_anthropic": 0}
            return {"memories": mems, "agents": agents, "stats": stats, "events": self.events[-30:]}


def client(port, stop):
    for _ in range(300):
        if stop(): return None
        try:
            s = socket.create_connection(("127.0.0.1", port), timeout=2); s.settimeout(1.0); return s
        except OSError:
            time.sleep(0.3)
    return None


def com3_loop(g, port, call, ollama, model, fallback, stop, prefix=""):
    s = client(port, stop)
    if not s:
        print(f"[obs] COM3 :{port} never connected"); return
    print(f"[obs] COM3 :{port} connected (LLM accelerator + tap)")
    buf = b""
    while not stop():
        try:
            d = s.recv(4096)
        except socket.timeout:
            continue
        except OSError:
            break
        if not d:
            break
        buf += d
        while b"\n" in buf:
            line, buf = buf.split(b"\n", 1)
            t = line.decode("utf-8", "ignore").strip()
            if not t.startswith("LLMREQ|"):
                continue
            parts = (t[len("LLMREQ|"):].split("|", 2) + ["", "", ""])[:3]
            mem, prompt, ctx = parts
            node = f"{prefix}:{mem}" if prefix else mem  # tag by instance in hub mode
            g.event("llm_request", f"{node}: {prompt}")
            for kv in ctx.split(","):
                if "=" in kv:
                    k, v = kv.split("=", 1); g.set_blob(node, k.strip(), v)
            try:
                key, value = call(ollama, model, mem, prompt, ctx); src = model
            except Exception as e:
                key, value = fallback(mem, prompt, ctx); src = f"fallback({type(e).__name__})"
            try:
                s.sendall(f"LLMRSP|{mem}|{key}|{value}\n".encode())  # wire keeps the guest's own name
            except OSError:
                pass
            g.set_blob(node, key, value); g.llm(key, value)
            g.event("llm_response", f"{node}: {key}={value} [{src}]")
            print(f"[obs] LLM {node}: {key}={value} [{src}]")


def mesh_hub(g, port, stop):
    """Multi-VM mesh: both guests connect here (COM2 client). We relay every HMSG
    to the OTHER guests (so the mesh still works) and record it per instance, so the
    observer shows each instance and the value propagating between them."""
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", port)); srv.listen(8); srv.settimeout(1.0)
    print(f"[obs] mesh hub listening on :{port} (guests connect as COM2 clients)")
    clients = {}   # sock -> label
    counter = [0]

    def handle(sock, label):
        buf = b""
        while not stop():
            try:
                d = sock.recv(4096)
            except socket.timeout:
                continue
            except OSError:
                break
            if not d:
                break
            buf += d
            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                t = line.decode("utf-8", "ignore").strip()
                if not t.startswith("HMSG|"):
                    continue
                p = t[5:].split("|", 2)
                if len(p) != 3:
                    continue
                mem, key, val = p
                g.set_blob(f"{label}:{mem}", key, val)
                g.event("mesh_broadcast", f"{label} -> {mem}: {key}={val}")
                print(f"[obs] MESH {label}: {mem}.{key}={val}")
                raw = (t + "\n").encode()
                for other, olabel in list(clients.items()):
                    if other is not sock:
                        try:
                            other.sendall(raw)              # the actual mesh relay
                        except OSError:
                            continue
                        g.set_blob(f"{olabel}:{mem}", key, val)   # peer now has it
                        g.propagate(f"{label}:{mem}", f"{olabel}:{mem}")
        clients.pop(sock, None)
        g.event("instance_leave", f"{label} left the mesh")
        try: sock.close()
        except OSError: pass

    while not stop():
        try:
            c, _ = srv.accept()
        except socket.timeout:
            continue
        except OSError:
            break
        counter[0] += 1
        label = f"vm{counter[0]}"
        c.settimeout(1.0)
        clients[c] = label
        g.event("instance_join", f"{label} joined the mesh")
        print(f"[obs] {label} joined the mesh")
        threading.Thread(target=handle, args=(c, label), daemon=True).start()


def com2_loop(g, port, stop):
    s = client(port, stop)
    if not s:
        print(f"[obs] COM2 :{port} never connected (ok if single VM has no mesh peer)"); return
    print(f"[obs] COM2 :{port} connected (mesh tap)")
    buf = b""
    while not stop():
        try:
            d = s.recv(4096)
        except socket.timeout:
            continue
        except OSError:
            break
        if not d:
            break
        buf += d
        while b"\n" in buf:
            line, buf = buf.split(b"\n", 1)
            t = line.decode("utf-8", "ignore").strip()
            if t.startswith("HMSG|"):
                p = t[5:].split("|", 2)
                if len(p) == 3:
                    mem, key, val = p
                    g.set_blob(mem, key, val)
                    g.event("mesh_broadcast", f"{mem}: {key}={val}")
                    print(f"[obs] MESH {mem}: {key}={val}")


def serve_http(g, port):
    graph = g

    class H(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            body = json.dumps(graph.snapshot()).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Access-Control-Allow-Origin", "*")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *a):
            pass

    srv = http.server.ThreadingHTTPServer(("127.0.0.1", port), H)
    print(f"[obs] serving observer snapshot at http://localhost:{port}/hive/snapshot")
    srv.serve_forever()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--com2", type=int, default=4444)
    ap.add_argument("--com3", type=int, default=4455)
    ap.add_argument("--http", type=int, default=8080)
    ap.add_argument("--hub", action="store_true",
                    help="multi-VM: run COM2 as a mesh hub (guests connect as clients, we relay + record)")
    ap.add_argument("--model", default="llama3.2:1b")
    ap.add_argument("--ollama", default="http://localhost:11434")
    args = ap.parse_args()

    b = load_bridge()
    g = Graph()
    stopped = False
    stop = lambda: stopped

    threading.Thread(target=serve_http, args=(g, args.http), daemon=True).start()
    prefix = "vm1" if args.hub else ""   # in hub mode the LLM device belongs to vm1
    threading.Thread(target=com3_loop, args=(g, args.com3, b.call_ollama, args.ollama, args.model, b.rule_fallback, stop, prefix), daemon=True).start()
    if args.hub:
        threading.Thread(target=mesh_hub, args=(g, args.com2, stop), daemon=True).start()
    else:
        threading.Thread(target=com2_loop, args=(g, args.com2, stop), daemon=True).start()
    print("[obs] bridge up (%s). point the observer at localhost:%d. Ctrl-C to stop."
          % ("hub/multi-VM" if args.hub else "single-VM", args.http))
    try:
        while True:
            time.sleep(1)
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
