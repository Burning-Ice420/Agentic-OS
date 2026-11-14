"""Scheduler metrics (blueprint 10.3)."""
from __future__ import annotations

import statistics

from .sim import Result


def jain(xs):
    xs = [x for x in xs if x > 0]
    if not xs:
        return 1.0
    return (sum(xs) ** 2) / (len(xs) * sum(x * x for x in xs))


def summarize(r: Result) -> dict:
    wfs = r.workflows.values()
    done = [w for w in wfs if w["ok"]]
    completion = [w["end"] - w["release"] for w in done]
    slowdown = [(w["end"] - w["release"]) / max(w["cp"], 1e-9) for w in done]
    missed = sum(1 for w in wfs if not w["ok"] or w["end"] > w["deadline"])
    waits = [t.start - t.ready for t in r.tasks if t.start >= 0 and t.ready >= 0]
    util = {res: (r.busy[res] / (r.slots[res] * r.horizon) if r.horizon > 0 else 0.0) for res in r.slots}
    return {
        "workflows": len(r.workflows),
        "succeeded": len(done),
        "failed": len(r.workflows) - len(done),
        "deadline_miss_rate": missed / max(len(r.workflows), 1),
        "completion_mean_s": statistics.fmean(completion) if completion else None,
        "completion_p95_s": sorted(completion)[int(0.95 * (len(completion) - 1))] if completion else None,
        "queue_wait_mean_s": statistics.fmean(waits) if waits else 0.0,
        "throughput_wf_per_s": len(done) / r.horizon if r.horizon else 0.0,
        "utilization": util,
        "fairness_jain_slowdown": jain(slowdown),
        "horizon_s": r.horizon,
    }
