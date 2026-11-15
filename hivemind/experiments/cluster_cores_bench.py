"""Cluster workers with several cores: the same 8 compute cores split four ways.

  8 VMs x 1 core | 4 VMs x 2 cores | 2 VMs x 4 cores | 1 VM x 8 cores
(a worker VM with k compute cores has k+1 vCPUs: its core 0 runs the shell and mesh.)

For each split: 48 prime jobs, least-loaded placement, 3 repetitions; 96 tiny matmul
jobs once. On the 2 x 4 split also: a worker corrupting 1 in 5 results with 25%
checking, and a worker VM killed mid-run. Every prime result is checked on the host.

  python experiments/cluster_cores_bench.py
"""
from __future__ import annotations

import os
import shutil
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.abspath(os.path.join(HERE, "..")))
from experiments import record  # noqa: E402
from experiments.cluster_bench import Cluster, check_results, cpu_during, run_window  # noqa: E402
from experiments.guest import kill_all_qemu  # noqa: E402

SRC = os.path.join(HERE, "..", "hivemind-os", "target", "x86_64-hivemind-os", "debug", "bootimage-hivemind-os.bin")
BOOT = r"C:\hivemind\boot_cc.bin"
SPLITS = [(8, 1), (4, 2), (2, 4), (1, 8)]   # (worker VMs, compute cores each)


def main():
    shutil.copy(SRC, BOOT)
    reps = 3
    runs, summary, failed = [], {}, []
    for nvm, k in SPLITS:
        label = f"{nvm} VMs x {k} cores"
        kill_all_qemu()
        cl = Cluster(nvm, ai=False, worker_cpus=k + 1 if k > 1 else 1, boot=BOOT)
        if not cl.ready():
            print(label, ": not every VM reached the prompt", flush=True)
            failed.append(label); cl.stop(); continue
        seen = cl.roles()
        cores = {w: v.get("cores") for w, v in cl.hub.snapshot()["load"].items()} if hasattr(cl.hub, "snapshot") else {}
        print(f"{label}: heartbeats from {seen}, cores reported {cores}", flush=True)
        run_no = 0

        def go(name, cmds, **kw):
            nonlocal run_no
            r = cl.run(name, cmds, **kw)
            r["split"] = label
            if r["finished"]:
                run_no += 1
                r["correctness"] = check_results(cl, run_no)
                r["window"] = run_window(cl, run_no)
                r["cpu_avg"] = cpu_during(cl, *r["window"])
            else:
                failed.append(f"{label}: {name}")
            runs.append(r)
            print(f"  {name:<30} {r.get('makespan_ms')} ms  per={r.get('per_worker')}  "
                  f"check={r.get('correctness')}  mism={r.get('mismatches')} re={r.get('reassigned')}", flush=True)
            return r

        cl.coord.type("cluster policy least", after=0.5)
        ms = []
        for rep in range(reps):
            r = go(f"prime48 rep={rep}", ["cluster run prime 48"])
            if r["finished"]:
                ms.append(int(r["makespan_ms"]))
        go("matmul96", ["cluster run matmul 96"])
        summary[label] = {"median_ms": sorted(ms)[len(ms) // 2] if ms else None, "all_ms": ms, "cores_reported": cores}

        if (nvm, k) == (2, 4):
            cl.vms["w2"].type("cluster evil on", after=1)
            go("evil w2, check 25%", ["cluster reset", "cluster verify 25", "cluster run prime 48"])
            cl.vms["w2"].type("cluster evil off", after=1)
            cl.coord.type("cluster verify 0", after=0.5)
            cl.coord.type("cluster reset", after=0.5)

            def kill_later():
                time.sleep(3)
                cl.hub.note("killing w2's VM")
                cl.vms["w2"].proc.kill()
            go("failure: kill w2", ["cluster run prime 64"], during=kill_later)
        cl.stop()

    kill_all_qemu()
    wrong = sum((r.get("correctness") or {}).get("wrong", 0) for r in runs)
    checked = sum((r.get("correctness") or {}).get("checked", 0) for r in runs)
    print("summary:", {k: v["median_ms"] for k, v in summary.items()}, flush=True)
    print(f"results checked {checked}, wrong accepted {wrong}", flush=True)
    path = record.write("cluster_cores", {"splits": SPLITS, "reps": reps, "policy": "least"},
                        {"summary": {"label": "measured", "value": summary},
                         "correctness": {"label": "measured", "value": {"checked": checked, "wrong": wrong}},
                         "runs": {"label": "measured", "value": runs}},
                        repetitions=reps, failed_runs=failed,
                        notes="Same 8 compute cores split across VMs; each worker VM's core 0 runs shell and mesh.")
    print("record:", path, flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
