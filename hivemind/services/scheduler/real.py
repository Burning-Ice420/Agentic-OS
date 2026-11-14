"""Execute task DAGs in real time with real local-model calls (one model slot,
as Ollama serves one request at a time on this machine). Durations are measured,
not simulated. CPU (rule) tasks are evaluated inline.
"""
from __future__ import annotations

import importlib.util
import os
import time

from . import policies
from .dag import Workflow

HERE = os.path.dirname(os.path.abspath(__file__))
BRIDGE = os.path.join(HERE, "..", "..", "hivemind-os", "hive-llm-bridge.py")

PROMPTS = [
    ("Is the system overheating? decide", "temp=95,unit=celsius"),
    ("Is the node overloaded? decide", "cpu=98,load=high"),
    ("Classify this log line as error or info", "log=connection refused by host"),
    ("Set the priority", "incident=production database down"),
]


def _bridge():
    spec = importlib.util.spec_from_file_location("bridge", BRIDGE)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


def run(workflows: list[Workflow], policy: str, model: str = "llama3.2:1b",
        ollama: str = "http://localhost:11434", options: dict | None = None) -> dict:
    call = _bridge().call_ollama
    ranks = {w.wid: w.ranks() for w in workflows}
    deps = {(w.wid, t): set(task.deps) for w in workflows for t, task in w.tasks.items()}
    released, ready, seq = set(), [], 0
    out = {"tasks": [], "workflows": {w.wid: {"release": w.release, "deadline": w.deadline, "end": None}
                                     for w in workflows}}
    left = {w.wid: len(w.tasks) for w in workflows}
    t0 = time.perf_counter()
    k = 0

    def now():
        return time.perf_counter() - t0

    def release_due():
        nonlocal seq
        for w in workflows:
            if w.wid not in released and w.release <= now():
                released.add(w.wid)
                for t, task in w.tasks.items():
                    if not task.deps:
                        seq += 1
                        ready.append((w.wid, t, now(), seq))

    wfs = {w.wid: w for w in workflows}
    while any(v > 0 for v in left.values()):
        release_due()
        if not ready:
            nxt = min((w.release for w in workflows if w.wid not in released), default=None)
            if nxt is None:
                break
            time.sleep(max(0.0, nxt - now()))
            continue
        tnow = now()
        best = min(ready, key=lambda r: policies.key(policy, now=tnow, ready_time=r[2], seq=r[3],
                                                      prio=wfs[r[0]].tasks[r[1]].prio,
                                                      deadline=wfs[r[0]].deadline, rank=ranks[r[0]][r[1]]))
        ready.remove(best)
        wid, tid, rdy, _ = best
        task = wfs[wid].tasks[tid]
        start = now()
        if task.resource == "model":
            prompt, ctx = PROMPTS[k % len(PROMPTS)]
            k += 1
            try:
                kv = call(ollama, model, wid, prompt, ctx, **({"options": options} if options else {}))
                ok = True
            except Exception as e:  # noqa: BLE001
                kv, ok = (type(e).__name__, ""), False
        else:
            kv, ok = ("rule", "fired"), True
        end = now()
        out["tasks"].append({"wid": wid, "tid": tid, "resource": task.resource, "ready": rdy,
                             "start": start, "end": end, "ok": ok, "answer": f"{kv[0]}={kv[1]}"})
        left[wid] -= 1
        for c in wfs[wid].children()[tid]:
            deps[(wid, c)].discard(tid)
            if not deps[(wid, c)]:
                seq += 1
                ready.append((wid, c, end, seq))
        if left[wid] == 0:
            out["workflows"][wid]["end"] = end
    wf = out["workflows"].values()
    done = [w for w in wf if w["end"] is not None]
    model_tasks = [t for t in out["tasks"] if t["resource"] == "model"]
    out["summary"] = {
        "policy": policy,
        "deadline_miss_rate": sum(1 for w in wf if w["end"] is None or w["end"] > w["deadline"]) / len(workflows),
        "completion_mean_s": sum(w["end"] - w["release"] for w in done) / max(len(done), 1),
        "queue_wait_mean_s": sum(t["start"] - t["ready"] for t in out["tasks"]) / max(len(out["tasks"]), 1),
        "model_call_mean_s": sum(t["end"] - t["start"] for t in model_tasks) / max(len(model_tasks), 1),
        "model_calls": len(model_tasks),
        "makespan_s": max((t["end"] for t in out["tasks"]), default=0.0),
    }
    return out
