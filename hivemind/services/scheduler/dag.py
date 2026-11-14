"""Task DAG model."""
from __future__ import annotations

from dataclasses import dataclass, field
from functools import lru_cache


@dataclass
class Task:
    tid: str
    deps: list[str]
    est: float            # duration the scheduler believes (seconds)
    true: float           # duration it actually takes (seconds; simulator only)
    resource: str         # "model" (LLM call) or "cpu" (reflex rule)
    prio: int = 0         # static priority, higher first
    max_retries: int = 1


@dataclass
class Workflow:
    wid: str
    release: float
    deadline: float       # absolute
    tasks: dict[str, Task] = field(default_factory=dict)

    def children(self) -> dict[str, list[str]]:
        ch = {t: [] for t in self.tasks}
        for t in self.tasks.values():
            for d in t.deps:
                ch[d].append(t.tid)
        return ch

    def validate(self):
        """Reject unknown dependencies and cycles (a DAG is required)."""
        for t in self.tasks.values():
            for d in t.deps:
                if d not in self.tasks:
                    raise ValueError(f"{self.wid}:{t.tid} depends on unknown {d}")
        seen, stack = set(), set()

        def visit(n):
            if n in stack:
                raise ValueError(f"{self.wid}: cycle through {n}")
            if n in seen:
                return
            stack.add(n)
            for d in self.tasks[n].deps:
                visit(d)
            stack.discard(n)
            seen.add(n)

        for n in self.tasks:
            visit(n)

    def ranks(self) -> dict[str, float]:
        """Upward rank: estimated length of the longest path from a task to the end."""
        ch = self.children()

        @lru_cache(maxsize=None)
        def r(t):
            return self.tasks[t].est + max((r(c) for c in ch[t]), default=0.0)

        return {t: r(t) for t in self.tasks}

    def critical_path(self) -> float:
        rk = self.ranks()
        return max((rk[t] for t, task in self.tasks.items() if not task.deps), default=0.0)
