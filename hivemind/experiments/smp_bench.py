"""Multi-core benchmark: one HiveMind guest with several vCPUs. Core 0 runs the
shell; the kernel places jobs on the other cores.

Phases:
  scaling    prime 48 on 1, 2, 4, 8 cores (dynamic placement), 3 repetitions
  kinds      hash, collatz, matmul (tiny jobs) on 1 and 8 cores
  imbalance  core 1 spends 60% of its time on a competing load; static vs dynamic
Prime and Collatz results are checked against a host computation; hash and
matmul results are checked for agreement between runs (the host cannot replay
60M sequential hash rounds quickly in Python).
Host CPU per vCPU comes from the QEMU process's thread times (psutil), in CPU seconds.

  python experiments/smp_bench.py            # full
  python experiments/smp_bench.py --smoke    # 4 vCPUs, one small run
"""
from __future__ import annotations

import argparse
import os
import re
import shutil
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.abspath(os.path.join(HERE, "..")))
from experiments import record  # noqa: E402
from experiments.cluster_bench import primes_in  # noqa: E402
from experiments.guest import Guest, kill_all_qemu  # noqa: E402

import psutil  # noqa: E402

SRC = os.path.join(HERE, "..", "hivemind-os", "target", "x86_64-hivemind-os", "debug", "bootimage-hivemind-os.bin")
BOOT = r"C:\hivemind\boot_smp.bin"


def collatz_best(lo: int, hi: int) -> int:
    """Same rule as cluster_core: longest chain start in [lo, hi), steps capped
    at 10000, encoded start*1000+min(steps,999); first start wins ties."""
    import numpy as np
    n = np.arange(lo, hi, dtype=np.uint64)
    steps = np.zeros(len(n), dtype=np.int64)
    alive = n != 1
    s = 0
    while alive.any() and s < 10_000:
        x = n[alive]
        n[alive] = np.where(x % 2 == 0, x // 2, 3 * x + 1)
        steps[alive] += 1
        alive = n != 1
        s += 1
    i = int(np.argmax(steps))  # first maximum
    return (lo + i) * 1000 + min(int(steps[i]), 999)


def expected(spec: str):
    k = spec.split(":")
    if k[0] == "prime":
        return primes_in(int(k[1]), int(k[2]))
    if k[0] == "collatz":
        return collatz_best(int(k[1]), int(k[2]))
    return None


class Bench:
    def __init__(self, cpus: int):
        shutil.copy(SRC, BOOT)
        self.g = Guest("smp_bench", BOOT, mem="128M", monitor_port=5595, smp=cpus).start()
        self.proc = psutil.Process(self.g.proc.pid)
        self.truth: dict[str, int] = {}
        self.seen: dict[str, set] = {}

    def lines(self):
        return [l for l in self.g.read_log() if l.startswith("SMP|")]

    def threads(self):
        return {t.id: t.user_time + t.system_time for t in self.proc.threads()}

    def cmd(self, c: str, after: float = 0.6):
        self.g.type(c, after=after)

    def run(self, label: str, kind: str, jobs: int, cores: int, sched: str, timeout: float = 400) -> dict:
        self.cmd(f"smp use {cores}")
        self.cmd(f"smp sched {sched}")
        before = sum(1 for l in self.lines() if "|done;" in l)
        th0, cpu0, t0 = self.threads(), self.proc.cpu_times(), time.time()
        self.g.type(f"smp run {kind} {jobs}", after=0.0)
        done = None
        while time.time() - t0 < timeout:
            d = [l for l in self.lines() if "|done;" in l]
            if len(d) > before:
                done = d[-1]
                break
            time.sleep(0.5)
        t1, th1, cpu1 = time.time(), self.threads(), self.proc.cpu_times()
        out = {"label": label, "kind": kind, "jobs": jobs, "cores": cores, "sched": sched, "finished": bool(done)}
        if not done:
            print(f"  {label}: TIMED OUT", flush=True)
            return out
        f = dict(p.split("=", 1) for p in done.split("|done;", 1)[1].split(";") if "=" in p)
        run_id = f["run"]
        wall = t1 - t0
        # host view: busiest QEMU threads (vCPUs) over the observation window
        # (CPU seconds, not percent: the window also holds typing and log latency)
        per = sorted((th1.get(i, 0) - th0.get(i, 0) for i in th1), reverse=True)
        busy = [int(x) for x in f["busy"].split(",") if x]
        out.update({
            "run": int(run_id), "ms": int(f["ms"]),
            "guest_busy_pct": busy,
            "guest_busy_s": round(sum(busy) / 100 * int(f["ms"]) / 1000, 2),
            "host_qemu_cpu_s": round((cpu1.user + cpu1.system) - (cpu0.user + cpu0.system), 2),
            "host_top_threads_cpu_s": [round(x, 2) for x in per[: cores + 2]],
            "host_window_s": round(wall, 2),
        })
        jobs_l = [l for l in self.lines() if f"|job;run={run_id};" in l]
        js, wrong, unchecked = [], 0, 0
        for l in jobs_l:
            g = dict(p.split("=", 1) for p in l.split("|job;", 1)[1].split(";") if "=" in p)
            spec, res = g["spec"], g["result"]
            js.append({"i": int(g["i"]), "core": int(g["core"]), "spec": spec, "result": res,
                       "start": int(g["start"]), "end": int(g["end"])})
            if spec not in self.truth:
                e = expected(spec)
                if e is not None:
                    self.truth[spec] = e
            if spec in self.truth:
                if str(self.truth[spec]) != res:
                    wrong += 1
            else:
                unchecked += 1
                self.seen.setdefault(spec, set()).add(res)
        out.update({"jobs_detail": js, "wrong_vs_host": wrong, "agreement_only": unchecked})
        print(f"  {label}: {out['ms']} ms, guest busy {out['guest_busy_s']} s {out['guest_busy_pct']}, host QEMU {out['host_qemu_cpu_s']} cpu-s, "
              f"wrong {wrong}", flush=True)
        return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--smoke", action="store_true")
    ap.add_argument("--cpus", type=int, default=9)
    ap.add_argument("--reps", type=int, default=3)
    a = ap.parse_args()
    cpus = 4 if a.smoke else a.cpus
    reps = 1 if a.smoke else a.reps
    kill_all_qemu()
    b = Bench(cpus)
    if not b.g.wait_ready(50):
        print("guest did not reach the prompt"); b.g.stop(); return 1
    boot = [l for l in b.g.read_log() if "[smp]" in l]
    print("boot:", boot, flush=True)
    workers = cpus - 1
    ns = [n for n in (1, 2, 4, 8, 16) if n < workers] + [workers]
    runs, failed = [], []

    def go(*args, **kw):
        r = b.run(*args, **kw)
        (runs if r["finished"] else failed).append(r)
        return r

    if a.smoke:
        go("smoke", "prime", 12, workers, "dynamic")
    else:
        for rep in range(reps):
            for n in ns:
                go(f"scaling n={n} rep={rep}", "prime", 48, n, "dynamic")
        for kind, jobs in (("hash", 16), ("collatz", 16), ("matmul", 48)):
            for n in (1, workers):
                go(f"kinds {kind} n={n}", kind, jobs, n, "dynamic")
        b.cmd("smp hog 1 60")
        for rep in range(reps):
            for sched in ("static", "dynamic"):
                go(f"imbalance {sched} rep={rep}", "prime", 48, workers, sched)
        b.cmd("smp hog 1 0")
        b.cmd("cpus", after=1.0)
        print(b.g.screen(), flush=True)

    b.g.stop()
    disagree = {s: sorted(v) for s, v in b.seen.items() if len(v) > 1}
    wrong = sum(r.get("wrong_vs_host", 0) for r in runs)
    checked = sum(len(r.get("jobs_detail", [])) - r.get("agreement_only", 0) for r in runs)
    total = sum(len(r.get("jobs_detail", [])) for r in runs)

    def med(label_prefix):
        v = sorted(r["ms"] for r in runs if r["label"].startswith(label_prefix))
        return v[len(v) // 2] if v else None

    summary = {}
    if not a.smoke:
        base = med("scaling n=1 ")
        summary["scaling_median_ms"] = {n: med(f"scaling n={n} ") for n in ns}
        summary["scaling_speedup"] = {n: round(base / summary["scaling_median_ms"][n], 2) for n in ns}
        summary["imbalance_median_ms"] = {s: med(f"imbalance {s}") for s in ("static", "dynamic")}
        summary["kinds_ms"] = {r["label"]: r["ms"] for r in runs if r["label"].startswith("kinds")}
    print("summary:", summary, flush=True)
    print(f"results: {total} jobs, {checked} checked against the host, {wrong} wrong; "
          f"{len(disagree)} specs disagreeing between runs", flush=True)
    path = record.write("smp_bench", {"vcpus": cpus, "compute_cores": workers, "reps": reps, "accel": "whpx",
                                      "boot_lines": boot},
                        {"summary": {"label": "measured", "value": summary},
                         "correctness": {"label": "measured", "value": {"jobs": total, "checked_vs_host": checked,
                                                                         "wrong": wrong, "disagreeing_specs": disagree}},
                         "runs": {"label": "measured", "value": runs}},
                        repetitions=reps, failed_runs=failed,
                        notes="Core 0 runs the shell and is not a compute core. Host CPU from QEMU thread times.")
    print("record:", path, flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
