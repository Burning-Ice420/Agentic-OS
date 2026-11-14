"""Scheduling policies. Each returns a sort key for a ready task; lowest key runs first.

fifo       order tasks became ready
priority   static priority, then FIFO
edf        earliest workflow deadline, then FIFO
dep_aware  least slack, where slack = deadline - now - (critical path remaining from
           this task, using estimates). Ties broken by longer remaining path.
slack_shed dep_aware, but work that can no longer meet its deadline (slack < 0) is
           served after all feasible work (avoids the EDF overload domino effect).
"""
from __future__ import annotations

POLICIES = ("fifo", "priority", "edf", "dep_aware", "slack_shed")


def key(policy: str, *, now: float, ready_time: float, seq: int, prio: int, deadline: float, rank: float):
    if policy == "fifo":
        return (ready_time, seq)
    if policy == "priority":
        return (-prio, ready_time, seq)
    if policy == "edf":
        return (deadline, ready_time, seq)
    if policy == "dep_aware":
        slack = deadline - now - rank
        return (slack, -rank, ready_time, seq)
    if policy == "slack_shed":
        slack = deadline - now - rank
        return (slack < 0, slack if slack >= 0 else -slack, -rank, ready_time, seq)
    raise ValueError(f"unknown policy {policy}")
