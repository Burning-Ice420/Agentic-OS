"""Discrete-event executor for task DAGs under resource limits.

Invariants (tested): a task never starts before all its dependencies finished;
never more tasks of a resource class run than there are slots; retries are bounded;
the same seed gives the same schedule.
"""
from __future__ import annotations

import heapq
import random
from dataclasses import dataclass, field

from . import policies
from .dag import Workflow


@dataclass
class TaskRun:
    wid: str
    tid: str
    ready: float = -1.0
    start: float = -1.0
    end: float = -1.0
    attempts: int = 0
    ok: bool = False


@dataclass
class Result:
    policy: str
    workflows: dict = field(default_factory=dict)   # wid -> {release, deadline, end, ok, cp}
    tasks: list = field(default_factory=list)       # TaskRun
    busy: dict = field(default_factory=dict)        # resource -> busy seconds
    slots: dict = field(default_factory=dict)
    horizon: float = 0.0
    trace: list = field(default_factory=list)       # (start, end, resource, wid, tid, ok)


def simulate(workflows: list[Workflow], slots: dict[str, int], policy: str, *,
             fail_prob: float = 0.0, backoff: float = 0.5, seed: int = 0) -> Result:
    if policy not in policies.POLICIES:
        raise ValueError(policy)
    rng = random.Random(seed)
    res = Result(policy=policy, slots=dict(slots), busy={r: 0.0 for r in slots})
    wfs = {w.wid: w for w in workflows}
    for w in workflows:
        w.validate()
    ranks = {w.wid: w.ranks() for w in workflows}
    runs = {(w.wid, t): TaskRun(w.wid, t) for w in workflows for t in w.tasks}
    remaining_deps = {(w.wid, t): set(task.deps) for w in workflows for t, task in w.tasks.items()}
    done_count = {w.wid: 0 for w in workflows}
    failed_wf = set()
    free = dict(slots)
    ready: list[tuple] = []  # (wid, tid, ready_time, seq)
    events: list[tuple] = []
    seq = 0

    def push(t, kind, payload):
        nonlocal seq
        seq += 1
        heapq.heappush(events, (t, seq, kind, payload))

    for w in workflows:
        res.workflows[w.wid] = {"release": w.release, "deadline": w.deadline, "end": None, "ok": False,
                                "cp": w.critical_path()}
        push(w.release, "release", w.wid)

    def make_ready(wid, tid, now):
        nonlocal seq
        seq += 1
        runs[(wid, tid)].ready = now
        ready.append((wid, tid, now, seq))

    def dispatch(now):
        progress = True
        while progress:
            progress = False
            for resource in slots:
                if free[resource] <= 0:
                    continue
                cands = [r for r in ready if wfs[r[0]].tasks[r[1]].resource == resource and r[0] not in failed_wf]
                if not cands:
                    continue
                best = min(cands, key=lambda r: policies.key(
                    policy, now=now, ready_time=r[2], seq=r[3], prio=wfs[r[0]].tasks[r[1]].prio,
                    deadline=wfs[r[0]].deadline, rank=ranks[r[0]][r[1]]))
                ready.remove(best)
                wid, tid = best[0], best[1]
                task = wfs[wid].tasks[tid]
                tr = runs[(wid, tid)]
                tr.attempts += 1
                if tr.start < 0:
                    tr.start = now
                free[resource] -= 1
                fails = rng.random() < fail_prob
                push(now + task.true, "finish", (wid, tid, not fails, now))
                progress = True

    now = 0.0
    while events:
        now, _, kind, payload = heapq.heappop(events)
        if kind == "release":
            w = wfs[payload]
            for t, task in w.tasks.items():
                if not task.deps:
                    make_ready(w.wid, t, now)
        elif kind == "finish":
            wid, tid, ok, started = payload
            task = wfs[wid].tasks[tid]
            free[task.resource] += 1
            res.busy[task.resource] += now - started
            res.trace.append((started, now, task.resource, wid, tid, ok))
            tr = runs[(wid, tid)]
            if ok:
                tr.ok, tr.end = True, now
                done_count[wid] += 1
                for c in wfs[wid].children()[tid]:
                    remaining_deps[(wid, c)].discard(tid)
                    if not remaining_deps[(wid, c)]:
                        make_ready(wid, c, now)
                if done_count[wid] == len(wfs[wid].tasks):
                    res.workflows[wid]["end"] = now
                    res.workflows[wid]["ok"] = True
            elif tr.attempts <= task.max_retries:
                push(now + backoff, "retry", (wid, tid))
            else:
                failed_wf.add(wid)
                res.workflows[wid]["end"] = now
        elif kind == "retry":
            wid, tid = payload
            if wid not in failed_wf:
                make_ready(wid, tid, now)
        dispatch(now)
    res.horizon = now
    res.tasks = list(runs.values())
    return res
