"""Hive cluster benchmark: up to 8 HiveMind workers + 1 coordinator, all real guests.

Phases (each a run decided by the coordinator's kernel, observed by the hub):
  scaling      prime 48 on 1, 2, 4, 8 workers (policy least)       speedup, CPU per VM
  granularity  matmul 96 (tiny jobs) on 1 and 8 workers            when the mesh dominates
  hetero       one worker shares its core with a CPU hog; rr vs least vs adaptive
  ai           12 model jobs on 1 and 4 workers (one Ollama behind them)
  pipeline     map (8 prime slices) -> reduce on coordinator -> AI report on a worker
  evil         one worker corrupts 1 in 5 results; coordinator re-runs 25% and quarantines it
  failure      a worker's VM is killed mid-run; its jobs are re-routed
Every compute result is checked against a host-side computation.

  python experiments/cluster_bench.py              # all phases
  python experiments/cluster_bench.py --smoke      # 2 workers, one small run
  python experiments/cluster_bench.py --live       # keep the dashboard up at the end
"""
from __future__ import annotations

import argparse
import math
import os
import re
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.abspath(os.path.join(HERE, "..")))
from experiments import record  # noqa: E402
from experiments.guest import Guest, kill_all_qemu  # noqa: E402
from services.cluster.hub import Hub, serve  # noqa: E402

BOOT = r"C:\hivemind\boot_p.bin"
WORK = r"C:\hivemind"
MODEL = "llama3.2:3b"
BRIDGE = os.path.join(HERE, "..", "hivemind-os", "hive-llm-bridge.py")

try:
    import psutil
except ImportError:
    psutil = None


# ── host-side truth ──
def primes_in(lo: int, hi: int, _cache={}) -> int:
    import numpy as np
    if "s" not in _cache or len(_cache["s"]) < hi:
        n = max(hi, 120_000_000)
        s = np.ones(n, dtype=bool)
        s[:2] = False
        for i in range(2, int(n ** 0.5) + 1):
            if s[i]:
                s[i * i::i] = False
        _cache["s"] = s
    return int(np.count_nonzero(_cache["s"][lo:hi]))


def expected(spec: str):
    k = spec.split(":")
    if k[0] == "prime":
        return primes_in(int(k[1]), int(k[2]))
    return None


# ── cluster bring-up ──
class Cluster:
    def __init__(self, n_workers: int, ai: bool):
        self.names = ["coord"] + [f"w{i}" for i in range(1, n_workers + 1)]
        self.vms, self.bridges, ports, pids = {}, [], {}, {}
        for i, n in enumerate(self.names):
            com2 = 4600 + i
            com3 = f"tcp:127.0.0.1:{4700 + i},server,nowait" if (ai and n != "coord") else None
            g = Guest(f"cl_{n}", BOOT, mem="64M", monitor_port=5600 + i,
                      com2=f"tcp:127.0.0.1:{com2},server,nowait", com3=com3).start()
            self.vms[n] = g
            ports[n] = com2
            pids[n] = g.proc.pid
        time.sleep(2)
        if ai:
            for i, n in enumerate(self.names):
                if n == "coord":
                    continue
                log = open(os.path.join(WORK, f"cl_{n}-bridge.txt"), "w")
                self.bridges.append(subprocess.Popen([sys.executable, "-u", BRIDGE, "--port", str(4700 + i),
                                                      "--model", MODEL], stdout=log, stderr=subprocess.STDOUT))
        self.hub = Hub(ports, pids)
        self.coord = self.vms["coord"]

    def ready(self) -> bool:
        time.sleep(55)
        ok = all("hive>" in g.screen() for g in self.vms.values())
        return ok

    def parallel(self, fn):
        ts = [threading.Thread(target=fn, args=(n, g)) for n, g in self.vms.items()]
        for t in ts:
            t.start()
        for t in ts:
            t.join()

    def roles(self):
        def f(n, g):
            g.type("cluster coord" if n == "coord" else f"cluster worker {n}", after=1)
        self.parallel(f)
        deadline = time.time() + 20
        while time.time() < deadline and len(self.hub.load) < len(self.names) - 1:
            time.sleep(0.5)
        return len(self.hub.load)

    def clines(self) -> list[str]:
        return [l for l in self.coord.read_log() if l.startswith("CLUSTER|")]

    def run(self, label: str, cmds: list[str], timeout: float = 300, during=None) -> dict:
        before = sum(1 for l in self.clines() if "|done;" in l)
        for c in cmds[:-1]:
            self.coord.type(c, after=0.5)
        self.hub.note(f"run {label}: {cmds[-1]}")
        t_typed = None
        self.coord.type(cmds[-1], after=0.0)
        t_typed = self.hub.now()
        if during:
            threading.Thread(target=during, daemon=True).start()
        deadline = time.time() + timeout
        done = None
        while time.time() < deadline:
            lines = [l for l in self.clines() if "|done;" in l]
            if len(lines) > before:
                done = lines[-1]
                break
            time.sleep(1)
        t_end = self.hub.now()
        out = {"label": label, "cmds": cmds, "finished": done is not None, "hub_t": [t_typed, t_end]}
        if done:
            f = dict(p.split("=", 1) for p in done.split("|done;", 1)[1].split(";") if "=" in p)
            out.update({k: f.get(k) for k in ("name", "policy", "jobs", "makespan_ms", "per_worker",
                                              "reassigned", "verified", "mismatches")})
        self.hub.note(f"run {label} finished: {out.get('makespan_ms')} ms" if done else f"run {label} TIMED OUT")
        return out

    def stop(self):
        for g in self.vms.values():
            g.stop()
        for b in self.bridges:
            b.kill()
        self.hub.close()


def check_results(cl: Cluster, run_no: int) -> dict:
    """Compare every accepted result of run `run_no` with the host computation."""
    snap = cl.hub.snapshot()
    spec = {e["job"]: e["spec"] for e in snap["events"] if e["type"] == "assign"}
    final = {}
    for l in cl.clines():
        m = re.search(r"\|job=(r%dj\d+);worker=(\w+);result=([^;]*);" % run_no, l)
        if m:
            final[m.group(1)] = (m.group(2), m.group(3))   # last accepted result wins
    checked = wrong = 0
    for job, (w, r) in final.items():
        s = spec.get(job, "")
        e = expected(s) if s else None
        if e is None:
            continue
        checked += 1
        if str(e) != r:
            wrong += 1
    return {"checked": checked, "wrong": wrong, "accepted": len(final)}


def run_window(cl: Cluster, run_no: int):
    ev = [e["t"] for e in cl.hub.snapshot()["events"] if e["job"].startswith(f"r{run_no}j")]
    return (min(ev), max(ev)) if ev else (0, 0)


def cpu_during(cl: Cluster, t0: float, t1: float) -> dict:
    rows = [r for r in cl.hub.snapshot()["cpu"] if t0 <= r["t"] <= t1]
    if not rows:
        return {}
    keys = [k for k in rows[0] if k != "t"]
    return {k: round(sum(r.get(k, 0) for r in rows) / len(rows), 1) for k in keys}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--smoke", action="store_true")
    ap.add_argument("--live", action="store_true")
    ap.add_argument("--workers", type=int, default=8)
    a = ap.parse_args()
    kill_all_qemu()
    nw = 2 if a.smoke else a.workers
    cl = Cluster(nw, ai=not a.smoke)
    serve(cl.hub)
    print("dashboard: http://127.0.0.1:8765", flush=True)
    if not cl.ready():
        print("not every VM reached the prompt"); cl.stop(); return 1
    print("heartbeats from", cl.roles(), "workers", flush=True)
    runs, run_no = [], 0

    def go(label, cmds, **kw):
        nonlocal run_no
        r = cl.run(label, cmds, **kw)
        if r["finished"]:
            run_no += 1
            r["run_no"] = run_no
            r["correctness"] = check_results(cl, run_no)
            r["window"] = run_window(cl, run_no)
            r["cpu_avg"] = cpu_during(cl, *r["window"])
        runs.append(r)
        print(f"  {label:<28} {r.get('makespan_ms')} ms  per={r.get('per_worker')}  "
              f"check={r.get('correctness')}  cpu={r.get('cpu_avg')}", flush=True)
        return r

    if a.smoke:
        go("smoke prime 8", ["cluster run prime 8"], timeout=120)
    else:
        # scaling
        for k in (1, 2, 4, 8):
            if k <= nw:
                go(f"scaling prime48 w={k}", [f"cluster use {k}", "cluster policy least", "cluster run prime 48"])
        # granularity
        go("granularity matmul96 w=1", ["cluster use 1", "cluster run matmul 96"])
        go(f"granularity matmul96 w={nw}", [f"cluster use {nw}", "cluster run matmul 96"])
        # heterogeneity: w1 shares one core with a CPU hog
        hog = None
        if psutil:
            hog = subprocess.Popen([sys.executable, "-c", "while True: pass"])
            core = [0]
            psutil.Process(hog.pid).cpu_affinity(core)
            psutil.Process(cl.vms["w1"].proc.pid).cpu_affinity(core)
            cl.hub.note("w1 now shares core 0 with a CPU hog")
            time.sleep(2)
        for pol in ("rr", "least", "adaptive"):
            go(f"hetero prime48 w=4 {pol}", ["cluster use 4", f"cluster policy {pol}", "cluster run prime 48"])
        if hog:
            hog.kill()
            psutil.Process(cl.vms["w1"].proc.pid).cpu_affinity(list(range(psutil.cpu_count())))
            cl.hub.note("hog stopped, w1 unpinned")
        # AI
        go("ai 12 w=1", ["cluster use 1", "cluster policy least", "cluster run ai 12"], timeout=400)
        go("ai 12 w=4", ["cluster use 4", "cluster run ai 12"], timeout=400)
        # pipeline
        pr = go("pipeline 8", [f"cluster use {nw}", "cluster run pipeline 8"], timeout=300)
        # verification cost with honest workers
        go("honest, check 25%", [f"cluster use {nw}", "cluster verify 25", "cluster run prime 48"])
        # untrusted worker: corrupts 1 result in 5
        cl.vms["w2"].type("cluster evil on", after=1)
        cl.hub.note("w2 switched to evil mode (1 in 5 results corrupted)")
        go("evil w2, check 25%", ["cluster reset", "cluster verify 25", "cluster run prime 48"])
        go("evil w2, check 100%", ["cluster reset", "cluster verify 100", "cluster run prime 48"])
        cl.vms["w2"].type("cluster evil off", after=1)
        cl.hub.note("w2 honest again")
        cl.coord.type("cluster verify 0", after=0.5)
        cl.coord.type("cluster reset", after=0.5)

        # failure: kill the last worker 4 s after the run starts
        victim = f"w{nw}"

        def kill_later():
            time.sleep(4)
            cl.hub.note(f"killing {victim}'s VM")
            cl.vms[victim].proc.kill()
        go(f"failure: kill {victim}", [f"cluster use {nw}", "cluster run prime 64"], during=kill_later)

    snap = cl.hub.snapshot()
    coord_log = cl.coord.read_log()
    report_lines = [l for l in coord_log if "CLUSTER|" in l]
    # pipeline outcome
    extra = {}
    if not a.smoke:
        sums = [l for l in report_lines if re.search(r"worker=coord;result=\d+", l)]
        extra["pipeline_sum_line"] = sums[-1] if sums else None
        extra["pipeline_truth"] = primes_in(50_000_000, 50_000_000 + 8 * 1_000_000)
        ai_res = [e for e in snap["events"] if e["type"] == "result" and not str(e["result"]).isdigit()]
        extra["ai_results_sample"] = [e["result"] for e in ai_res[-6:]]
    path = record.write("cluster_bench", {"workers": nw, "model": MODEL, "smoke": a.smoke},
                        {"runs": {"label": "measured", "value": runs},
                         "extra": {"label": "measured", "value": extra},
                         "hub": {"label": "measured", "value": {"events": snap["events"], "cpu": snap["cpu"],
                                                                "notes": snap["notes"], "cores": snap["cores"]}},
                         "coord_cluster_lines": {"label": "measured", "value": report_lines}},
                        failed_runs=[r["label"] for r in runs if not r["finished"]], repetitions=1,
                        notes="one repetition per configuration; host CPU from psutil per QEMU process")
    print("record:", path, flush=True)
    if a.live:
        print("dashboard stays up; Ctrl+C to stop", flush=True)
        try:
            while True:
                time.sleep(1)
        except KeyboardInterrupt:
            pass
    cl.stop()
    return 0


if __name__ == "__main__":
    sys.exit(main())
