"""Seeded synthetic agent workloads.

Shapes mirror HiveMind patterns:
  chain    sensor rule (cpu) -> coordinator model call -> actuator rule (cpu)
  fanout   planner model call -> k executor model calls -> merge rule (cpu)
  single   one model call

Model-call durations are drawn from a lognormal fitted to measured local-model
latencies when available (label: derived), otherwise to the defaults below.
Rule tasks take about one kernel tick (55 ms).
"""
from __future__ import annotations

import json
import math
import os
import random

from .dag import Task, Workflow

DEFAULT_LLM_MEAN, DEFAULT_LLM_SD = 3.5, 0.6   # seconds, from ai-bench (3B warm), see BENCHMARKS
RULE = 0.055


def measured_latency(path_glob_dir: str | None = None):
    """Mean/sd of measured model-call latencies from ai-bench-result.json, if present."""
    p = os.path.join(os.path.dirname(__file__), "..", "..", "hivemind-os", "ai-bench-result.json")
    try:
        rows = json.load(open(p, encoding="utf-8"))["rows"]
        xs = [r["lat_s"] for r in rows if r.get("lat_s")]
        if len(xs) >= 5:
            m = sum(xs) / len(xs)
            sd = (sum((x - m) ** 2 for x in xs) / (len(xs) - 1)) ** 0.5
            return m, sd, "derived"
    except (OSError, KeyError, ValueError):
        pass
    return DEFAULT_LLM_MEAN, DEFAULT_LLM_SD, "default"


def _lognormal(rng, mean, sd):
    s2 = math.log(1 + (sd / mean) ** 2)
    return rng.lognormvariate(math.log(mean) - s2 / 2, math.sqrt(s2))


def generate(seed: int, n: int, rate: float, *, llm_mean=None, llm_sd=None, tight_frac=0.5,
             shape_mix=("chain", "fanout", "single")) -> list[Workflow]:
    rng = random.Random(seed)
    if llm_mean is None:
        llm_mean, llm_sd, _ = measured_latency()
    t = 0.0
    out = []
    for i in range(n):
        t += rng.expovariate(rate)
        shape = rng.choice(shape_mix)
        prio = rng.randint(0, 2)
        tasks = {}

        def add(tid, deps, resource):
            true = _lognormal(rng, llm_mean, llm_sd) if resource == "model" else RULE
            est = llm_mean if resource == "model" else RULE
            tasks[tid] = Task(tid, deps, est, true, resource, prio, max_retries=1)

        if shape == "chain":
            add("sense", [], "cpu"); add("coord", ["sense"], "model"); add("act", ["coord"], "cpu")
        elif shape == "fanout":
            k = rng.randint(2, 4)
            add("plan", [], "model")
            for j in range(k):
                add(f"exec{j}", ["plan"], "model")
            add("merge", [f"exec{j}" for j in range(k)], "cpu")
        else:
            add("decide", [], "model")
        w = Workflow(f"wf{i}", t, 0.0, tasks)
        cp = w.critical_path()
        factor = rng.uniform(1.3, 2.0) if rng.random() < tight_frac else rng.uniform(3.0, 6.0)
        w.deadline = t + factor * cp
        out.append(w)
    return out
