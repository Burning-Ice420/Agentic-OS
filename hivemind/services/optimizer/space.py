"""Search spaces and hard constraints.

Only parameters that can be changed safely and reset reproducibly are registered.
Security-relevant settings (capability enforcement, provenance) are NOT tunable:
the validator rejects any configuration that touches them, so an optimizer can
never trade security for speed.
"""
from __future__ import annotations

import itertools
import random
from dataclasses import dataclass, field

FORBIDDEN_KEYS = {"cap_enforce", "prov_enabled", "capabilities"}


@dataclass
class Param:
    name: str
    values: list               # discrete, ordered values

    def encode(self, v) -> list[float]:
        if all(isinstance(x, (int, float)) for x in self.values):
            lo, hi = min(self.values), max(self.values)
            return [0.0 if hi == lo else (v - lo) / (hi - lo)]
        return [1.0 if v == x else 0.0 for x in self.values]


@dataclass
class Space:
    name: str
    params: list[Param]
    constraints: list = field(default_factory=list)   # callables cfg -> str | None (reason)

    def size(self) -> int:
        n = 1
        for p in self.params:
            n *= len(p.values)
        return n

    def all(self):
        for combo in itertools.product(*[p.values for p in self.params]):
            yield dict(zip([p.name for p in self.params], combo))

    def sample(self, rng: random.Random) -> dict:
        return {p.name: rng.choice(p.values) for p in self.params}

    def encode(self, cfg: dict) -> list[float]:
        out = []
        for p in self.params:
            out += p.encode(cfg[p.name])
        return out

    def validate(self, cfg: dict):
        """Return None if the config may run, else the reason it is rejected."""
        bad = FORBIDDEN_KEYS & set(cfg)
        if bad:
            return f"touches security settings {sorted(bad)}"
        for p in self.params:
            if cfg.get(p.name) not in p.values:
                return f"{p.name}={cfg.get(p.name)!r} outside registered values"
        for c in self.constraints:
            r = c(cfg)
            if r:
                return r
        return None


def key(cfg: dict) -> tuple:
    return tuple(sorted(cfg.items()))
