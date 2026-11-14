"""Phase 3: scheduler evaluation.

  sim   4 policies x 3 load levels x 10 seeds (simulated; model-call durations derived
        from measured local-model latency), mean and 95% CI.
  real  same small workload executed with real local-model calls per policy (measured).

  python experiments/phase3_sched.py sim
  python experiments/phase3_sched.py real --reps 2
"""
from __future__ import annotations

import argparse
import os
import statistics
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.abspath(os.path.join(HERE, "..")))
from experiments import record  # noqa: E402
from services.scheduler.metrics import summarize  # noqa: E402
from services.scheduler.policies import POLICIES  # noqa: E402
from services.scheduler.sim import simulate  # noqa: E402
from services.scheduler.workload import generate, measured_latency  # noqa: E402

try:
    from scipy import stats as sps
except ImportError:  # pragma: no cover
    sps = None


def ci95(xs):
    xs = [x for x in xs if x is not None]
    if len(xs) < 2:
        return (xs[0] if xs else None, None)
    m, sd = statistics.fmean(xs), statistics.stdev(xs)
    t = sps.t.ppf(0.975, len(xs) - 1) if sps else 2.262
    return (m, t * sd / len(xs) ** 0.5)


def sim(seeds=10, n=200):
    mean, sd, src = measured_latency()
    slots = {"model": 2, "cpu": 4}
    # mean model work per workflow ~ (1 + (1+3) + 1)/3 = 2 calls
    capacity = slots["model"] / (2.0 * mean)
    loads = {"light_0.5x": 0.5, "medium_0.85x": 0.85, "overload_1.3x": 1.3}
    table = {}
    t_dispatch = []
    for lname, f in loads.items():
        for pol in POLICIES:
            runs = []
            for s in range(seeds):
                ws = generate(seed=1000 + s, n=n, rate=f * capacity, llm_mean=mean, llm_sd=sd)
                t0 = time.perf_counter()
                r = simulate(ws, slots, pol, fail_prob=0.05, seed=s)
                t_dispatch.append((time.perf_counter() - t0) / max(len(r.trace), 1))
                runs.append(summarize(r))
            table[(lname, pol)] = {
                k: ci95([r[k] for r in runs])
                for k in ("deadline_miss_rate", "completion_mean_s", "completion_p95_s",
                          "queue_wait_mean_s", "throughput_wf_per_s", "fairness_jain_slowdown")
            }
            table[(lname, pol)]["model_util"] = ci95([r["utilization"]["model"] for r in runs])
    print(f"model-call latency: mean={mean:.2f}s sd={sd:.2f}s ({src}); capacity={capacity:.3f} wf/s; slots={slots}")
    hdr = f"{'load':<14}{'policy':<11}{'miss rate':>18}{'completion s':>18}{'wait s':>16}{'fairness':>14}"
    print(hdr)
    for (l, p), m in table.items():
        def f2(x):
            return f"{x[0]:.3f}±{x[1]:.3f}" if x[1] is not None else f"{x[0]}"
        print(f"{l:<14}{p:<11}{f2(m['deadline_miss_rate']):>18}{f2(m['completion_mean_s']):>18}"
              f"{f2(m['queue_wait_mean_s']):>16}{f2(m['fairness_jain_slowdown']):>14}")
    metrics = {f"{l}/{p}": {"label": "simulated", "value": {k: {"mean": v[0], "ci95": v[1]} for k, v in m.items()}}
               for (l, p), m in table.items()}
    metrics["scheduler_overhead_s_per_dispatch"] = {"label": "measured",
                                                    "value": statistics.fmean(t_dispatch)}
    path = record.write("phase3_sim", {"slots": slots, "n_workflows": n, "fail_prob": 0.05,
                                       "loads": loads, "latency_source": src, "llm_mean": mean, "llm_sd": sd},
                        metrics, seed="1000+s, s=0..9", repetitions=seeds,
                        notes="durations derived from measured model latency; results are simulated")
    print("record:", path)


def real(reps=2, n=8, model="llama3.2:1b"):
    from services.scheduler import real as rx
    out = {}
    failed = []
    for p in ("fifo", "edf", "dep_aware"):
        out[p] = []
        for rep in range(reps):
            ws = generate(seed=77 + rep, n=n, rate=0.22, llm_mean=3.0, llm_sd=0.5, shape_mix=("chain", "single", "fanout"))
            for w in ws:  # keep the real run short: cap fan-out at 2 executors
                for t in [t for t in list(w.tasks) if t.startswith("exec") and t not in ("exec0", "exec1")]:
                    del w.tasks[t]
                if "merge" in w.tasks:
                    w.tasks["merge"].deps = [d for d in w.tasks["merge"].deps if d in w.tasks]
            try:
                r = rx.run(ws, p, model=model)
                out[p].append(r["summary"])
                print(p, rep, {k: round(v, 3) if isinstance(v, float) else v for k, v in r["summary"].items()}, flush=True)
            except Exception as e:  # noqa: BLE001
                failed.append({"policy": p, "rep": rep, "reason": repr(e)})
    metrics = {p: {"label": "measured", "value": {
        k: {"mean": statistics.fmean([s[k] for s in v]), "runs": [s[k] for s in v]}
        for k in ("deadline_miss_rate", "completion_mean_s", "queue_wait_mean_s", "model_call_mean_s", "makespan_s")}}
        for p, v in out.items() if v}
    path = record.write("phase3_real", {"model": model, "workflows": n, "rate_wf_per_s": 0.22, "model_slots": 1},
                        metrics, seed="77+rep", repetitions=reps, failed_runs=failed,
                        warmup="one model call before first run",
                        notes="real local-model calls; one model slot (Ollama serial)")
    print("record:", path)


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("mode", choices=["sim", "real"])
    ap.add_argument("--reps", type=int, default=2)
    a = ap.parse_args()
    if a.mode == "sim":
        sim()
    else:
        import importlib.util
        spec = importlib.util.spec_from_file_location("b", os.path.join(HERE, "..", "hivemind-os", "hive-llm-bridge.py"))
        b = importlib.util.module_from_spec(spec); spec.loader.exec_module(b)
        b.call_ollama("http://localhost:11434", "llama3.2:1b", "warm", "say ok", "")  # warm-up
        real(reps=a.reps)
