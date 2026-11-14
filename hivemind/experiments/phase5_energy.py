"""Phase 5: energy-aware execution, measured with RAPL (Windows EMI).

Policies over the 13-task decision benchmark (ai-bench.py TASKS):
  model_3b       every task -> llama3.2:3b
  model_1b       every task -> llama3.2:1b (smaller-model routing)
  reflex_3b      numeric-threshold tasks (5) -> deterministic rule (the kernel's reflex
                 tier); the other 8 -> 3b
  batched_3b     every task -> 3b, 4-5 tasks per prompt (fewer calls)
  reflex_batched reflex for the 5 threshold tasks + batched 3b for the rest

Metric: idle-subtracted CPU-package joules per successful task, plus latency and
accuracy. Repeated with the policy order shuffled per repetition.

  python experiments/phase5_energy.py --reps 3
"""
from __future__ import annotations

import argparse
import importlib.util
import os
import random
import re
import statistics
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.abspath(os.path.join(HERE, "..")))
from experiments import calllog, record  # noqa: E402
from services.energy.meter import EnergyMeter, carbon_grams, detect, idle_baseline, window  # noqa: E402

ROOT = os.path.join(HERE, "..", "hivemind-os")
OLLAMA = "http://localhost:11434"


def _load(name, file):
    s = importlib.util.spec_from_file_location(name, os.path.join(ROOT, file))
    m = importlib.util.module_from_spec(s); s.loader.exec_module(m)
    return m


bridge = _load("bridge", "hive-llm-bridge.py")
aib = _load("aib", "ai-bench.py")
TASKS = aib.TASKS

# Rule-expressible tasks: a numeric threshold decides them. Mirrors kernel rules
# such as `agent rule <id> temp gt:80 ...`.
RULES = {
    "overheat_high": ("temp", 80, "is_overheating"),
    "overheat_low": ("temp", 80, "is_overheating"),
    "cpu_saturation": ("cpu", 90, "overloaded"),
    "cpu_idle": ("cpu", 90, "overloaded"),
    "disk_full": ("disk_used", 90, "need_attention"),
}


def rule_decide(task):
    var, thr, key = RULES[task["name"]]
    m = re.search(rf"{var}=(\d+)", task["ctx"])
    return key, ("true" if m and int(m.group(1)) > thr else "false")


def single_calls(model, tasks):
    out = []
    for t in tasks:
        s0 = time.perf_counter()
        try:
            k, v = bridge.call_ollama(OLLAMA, model, t["name"], t["prompt"], t["ctx"])
        except Exception as e:  # noqa: BLE001
            k, v = "error", type(e).__name__
        calllog.log(source="phase5_energy", model=model, num_predict=24, temperature=0.1, fewshot=False,
                    prompt_len=len(t["prompt"]), ctx_len=len(t["ctx"]), out_len=len(f"{k}={v}"),
                    latency_s=time.perf_counter() - s0, task=t["name"])
        out.append((t, k, v))
    return out


BATCH_SYS = ("You are a decision engine inside an OS kernel. For EACH numbered item reply with exactly one "
             "line of the form '<number>: key=value' (snake_case key, one-word value). No other text.")


def batched_calls(model, tasks, size=5):
    out = []
    for i in range(0, len(tasks), size):
        chunk = tasks[i:i + size]
        body = "\n".join(f"{j + 1}. state: {t['ctx']} | task: {t['prompt']}" for j, t in enumerate(chunk))
        import json
        import urllib.request
        req = urllib.request.Request(f"{OLLAMA}/api/chat", data=json.dumps({
            "model": model, "stream": False, "options": {"temperature": 0.1, "num_predict": 24 * len(chunk)},
            "messages": [{"role": "system", "content": BATCH_SYS}, {"role": "user", "content": body}]}).encode(),
            headers={"Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=bridge.LLM_TIMEOUT) as r:
                text = json.loads(r.read().decode()).get("message", {}).get("content", "")
        except Exception as e:  # noqa: BLE001
            text = f"error {e}"
        lines = {}
        for ln in text.splitlines():
            m = re.match(r"\s*(\d+)\s*[:.)-]\s*([A-Za-z][\w\-]*)\s*=\s*([^\s,;|]+)", ln)
            if m:
                lines[int(m.group(1))] = (m.group(2), m.group(3))
        for j, t in enumerate(chunk):
            k, v = lines.get(j + 1, ("missing", "missing"))
            out.append((t, k, v))
    return out


def run_policy(name):
    rule_t = [t for t in TASKS if t["name"] in RULES]
    model_t = [t for t in TASKS if t["name"] not in RULES]
    res, calls = [], 0
    if name == "model_3b":
        res = single_calls("llama3.2:3b", TASKS); calls = len(TASKS)
    elif name == "model_1b":
        res = single_calls("llama3.2:1b", TASKS); calls = len(TASKS)
    elif name == "reflex_3b":
        res = [(t, *rule_decide(t)) for t in rule_t] + single_calls("llama3.2:3b", model_t); calls = len(model_t)
    elif name == "batched_3b":
        res = batched_calls("llama3.2:3b", TASKS); calls = -(-len(TASKS) // 5)
    elif name == "reflex_batched":
        res = [(t, *rule_decide(t)) for t in rule_t] + batched_calls("llama3.2:3b", model_t)
        calls = -(-len(model_t) // 5)
    ok = sum(1 for t, k, v in res if aib.scored(t, k, v))
    safety = next(((k, v) for t, k, v in res if t["name"] == "safety_block"), None)
    return ok, calls, safety


POLICIES = ["model_3b", "model_1b", "reflex_3b", "batched_3b", "reflex_batched"]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--reps", type=int, default=3)
    ap.add_argument("--carbon-intensity", type=float, default=None, help="gCO2e/kWh (needs --carbon-source/--carbon-time)")
    ap.add_argument("--carbon-source")
    ap.add_argument("--carbon-time")
    a = ap.parse_args()

    det = detect()
    print("energy sources:", det, flush=True)
    meter = EnergyMeter()
    for mdl in ("llama3.2:3b", "llama3.2:1b"):
        bridge.call_ollama(OLLAMA, mdl, "warm", "say ok", "")
    idle = idle_baseline(meter, seconds=20, reps=3)
    print(f"idle package power: {idle['watts_mean']:.2f} ± {idle['watts_sd']:.2f} W", flush=True)

    rows, failed = [], []
    rng = random.Random(42)
    for rep in range(a.reps):
        order = POLICIES[:]
        rng.shuffle(order)
        for p in order:
            warm = "llama3.2:1b" if p == "model_1b" else "llama3.2:3b"
            bridge.call_ollama(OLLAMA, warm, "warm", "say ok", "")   # load model outside the window
            try:
                e0 = meter.read(); t0 = time.perf_counter()
                ok, calls, safety = run_policy(p)
                t1 = time.perf_counter(); e1 = meter.read()
            except Exception as e:  # noqa: BLE001
                failed.append({"rep": rep, "policy": p, "reason": repr(e)}); continue
            w = window(e0, e1)["rapl_package0_pkg"]
            net = w["joules"] - idle["watts_mean"] * w["seconds"]
            row = {"rep": rep, "policy": p, "successes": ok, "tasks": len(TASKS), "model_calls": calls,
                   "workload_s": t1 - t0, "window_s": w["seconds"], "pkg_joules": w["joules"],
                   "net_joules": net, "net_j_per_success": net / ok if ok else None,
                   "cores_joules": window(e0, e1)["rapl_package0_pp0"]["joules"], "safety_answer": safety}
            rows.append(row)
            print(f"rep{rep} {p:<15} ok {ok:>2}/13 calls {calls:>2} {row['workload_s']:6.1f}s "
                  f"net {net:7.1f} J  {row['net_j_per_success'] and round(row['net_j_per_success'], 1)} J/success  "
                  f"safety={safety}", flush=True)
    meter.close()

    summary = {}
    for p in POLICIES:
        rs = [r for r in rows if r["policy"] == p and r["net_j_per_success"] is not None]
        if not rs:
            continue

        def ms(k):
            xs = [r[k] for r in rs]
            return {"mean": statistics.fmean(xs), "sd": statistics.stdev(xs) if len(xs) > 1 else 0.0}
        summary[p] = {k: ms(k) for k in ("net_j_per_success", "net_joules", "successes", "workload_s", "model_calls")}
        summary[p]["carbon"] = carbon_grams(summary[p]["net_joules"]["mean"], a.carbon_intensity,
                                            a.carbon_source, a.carbon_time)
    print("\npolicy           J/success (net)      accuracy      seconds")
    for p, s in summary.items():
        print(f"{p:<16} {s['net_j_per_success']['mean']:7.1f} ± {s['net_j_per_success']['sd']:5.1f}   "
              f"{s['successes']['mean']:5.1f}/13   {s['workload_s']['mean']:6.1f} ± {s['workload_s']['sd']:4.1f}")
    metrics = {"idle_baseline_w": {"label": "measured", "value": idle},
               "per_run": {"label": "measured", "value": rows},
               "summary": {"label": "derived", "value": summary},
               "sources": {"label": "measured", "value": det}}
    path = record.write("phase5_energy", {"policies": POLICIES, "tasks": len(TASKS), "rail": "rapl_package0_pkg",
                                          "batch_size": 5, "rules": {k: v[:2] for k, v in RULES.items()}},
                        metrics, seed=42, repetitions=a.reps,
                        warmup="model loaded by one untimed call before each window",
                        failed_runs=failed,
                        notes="CPU-package RAPL energy via Windows EMI, idle-subtracted; not wall power. "
                              "Background processes on the laptop add noise (see sd).")
    print("record:", path)


if __name__ == "__main__":
    main()
