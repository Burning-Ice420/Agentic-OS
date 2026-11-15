"""Why 8 cores gave 5.7x, not 8x: rerun the multi-core prime workload with the QEMU
process pinned to chosen host CPUs, and compare the time per job.

Host (i5-13420H, from GetSystemCpuSetInformation): logical CPUs 0-7 are 4 performance
cores with 2 hyperthreads each (pairs 0/1, 2/3, 4/5, 6/7); 8-11 are 4 efficiency cores.
The guest has 9 vCPUs; core 0 (the shell) is idle during runs, so the pinned set holds
the compute vCPUs. 48 prime jobs, dynamic placement, 3 repetitions per configuration.

  python experiments/smp_pinning.py
"""
from __future__ import annotations

import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.abspath(os.path.join(HERE, "..")))
from experiments import record  # noqa: E402
from experiments.guest import kill_all_qemu  # noqa: E402
from experiments.smp_bench import Bench  # noqa: E402

P_ONE = [0, 2, 4, 6]          # one thread on each performance core
P_ALL = list(range(8))        # both threads on each performance core
E = [8, 9, 10, 11]            # efficiency cores
CONFIGS = [
    # label, host CPUs, compute cores used
    ("1 core on a P core", [2], 1),
    ("1 core on an E core", [9], 1),
    ("4 cores, 4 P cores (no hyperthread sharing)", P_ONE, 4),
    ("4 cores, 2 P cores x 2 hyperthreads", [0, 1, 2, 3], 4),
    ("4 cores, 4 E cores", E, 4),
    ("8 cores, 4 P cores x 2 hyperthreads", P_ALL, 8),
    ("8 cores, 4 P + 4 E (one per physical core)", P_ONE + E, 8),
    ("8 cores, unpinned (all 12)", list(range(12)), 8),
]


def main():
    reps = 3
    kill_all_qemu()
    b = Bench(9)
    if not b.g.wait_ready(50):
        print("guest did not reach the prompt"); b.g.stop(); return 1
    rows, failed = [], []
    for rep in range(reps):
        for label, cpus, n in CONFIGS:
            b.proc.cpu_affinity(cpus)
            time.sleep(0.5)
            r = b.run(f"{label} rep={rep}", "prime", 48, n, "dynamic")
            if not r["finished"]:
                failed.append(r); continue
            per_job = [j["end"] - j["start"] for j in r["jobs_detail"]]
            r["host_cpus"] = cpus
            r["config"] = label
            r["mean_job_ms"] = round(sum(per_job) / len(per_job), 1)
            rows.append(r)
            print(f"    mean job {r['mean_job_ms']} ms", flush=True)
    b.proc.cpu_affinity(list(range(12)))
    b.g.stop()

    def med(v):
        v = sorted(v); return v[len(v) // 2]
    summary = {}
    for label, cpus, n in CONFIGS:
        rs = [r for r in rows if r["config"] == label]
        if rs:
            summary[label] = {"cores": n, "host_cpus": cpus, "median_ms": med([r["ms"] for r in rs]),
                              "median_job_ms": med([r["mean_job_ms"] for r in rs]),
                              "wrong": sum(r["wrong_vs_host"] for r in rs)}
    for k, v in summary.items():
        print(f"{k:<48} {v['median_ms']:>7} ms  job {v['median_job_ms']:>6} ms  wrong {v['wrong']}")
    path = record.write("smp_pinning", {"vcpus": 9, "reps": reps, "workload": "prime 48, dynamic",
                                        "host_cpu_map": {"P_threads": P_ALL, "E": E}},
                        {"summary": {"label": "measured", "value": summary},
                         "runs": {"label": "measured", "value": rows}},
                        repetitions=reps, failed_runs=failed,
                        notes="QEMU process affinity set with psutil before each run; core 0 vCPU idle during runs.")
    print("record:", path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
