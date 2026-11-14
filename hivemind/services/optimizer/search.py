"""Search methods. All minimise `objective(cfg) -> float` (lower is better).

Budget = number of objective evaluations. Proposals rejected by the validator are
counted as invalid and do not consume budget (they cost nothing to reject).
"""
from __future__ import annotations

import math
import random
import time

import numpy as np
from scipy.stats import norm

from .space import Space, key


class Trace:
    def __init__(self, method: str):
        self.method = method
        self.evals: list[tuple[dict, float, float]] = []   # cfg, value, seconds
        self.invalid: list[tuple[dict, str]] = []
        self.overhead_s = 0.0

    def best(self):
        return min(self.evals, key=lambda e: e[1]) if self.evals else None

    def curve(self) -> list[float]:
        out, b = [], math.inf
        for _, v, _ in self.evals:
            b = min(b, v)
            out.append(b)
        return out

    def cost_s(self) -> float:
        return sum(s for *_, s in self.evals) + self.overhead_s

    def to_dict(self) -> dict:
        b = self.best()
        return {"method": self.method, "evaluations": len(self.evals), "invalid_proposals": len(self.invalid),
                "best_value": b[1] if b else None, "best_config": b[0] if b else None,
                "curve": self.curve(), "eval_cost_s": sum(s for *_, s in self.evals),
                "optimizer_overhead_s": self.overhead_s,
                "invalid_reasons": sorted({r for _, r in self.invalid})}


def _eval(tr: Trace, objective, cfg):
    t0 = time.perf_counter()
    v = float(objective(cfg))
    tr.evals.append((cfg, v, time.perf_counter() - t0))
    return v


def random_search(space: Space, objective, budget: int, seed: int = 0) -> Trace:
    rng, tr, seen = random.Random(seed), Trace("random"), set()
    tries = 0
    while len(tr.evals) < budget and tries < budget * 50:
        tries += 1
        cfg = space.sample(rng)
        if key(cfg) in seen:
            continue
        seen.add(key(cfg))
        r = space.validate(cfg)
        if r:
            tr.invalid.append((cfg, r))
            continue
        _eval(tr, objective, cfg)
    return tr


def grid_search(space: Space, objective, budget: int) -> Trace | None:
    """Exhaustive search; returns None when the space exceeds the budget (infeasible)."""
    if space.size() > budget:
        return None
    tr = Trace("grid")
    for cfg in space.all():
        r = space.validate(cfg)
        if r:
            tr.invalid.append((cfg, r))
            continue
        _eval(tr, objective, cfg)
    return tr


def _gp_posterior(X, y, Xs, ls, noise=1e-4):
    def k(a, b):
        d = ((a[:, None, :] - b[None, :, :]) ** 2).sum(-1)
        return np.exp(-0.5 * d / ls ** 2)
    K = k(X, X) + noise * np.eye(len(X))
    L = np.linalg.cholesky(K)
    alpha = np.linalg.solve(L.T, np.linalg.solve(L, y))
    Ks = k(X, Xs)
    mu = Ks.T @ alpha
    v = np.linalg.solve(L, Ks)
    var = np.clip(1.0 - (v ** 2).sum(0), 1e-12, None)
    lml = -0.5 * y @ alpha - np.log(np.diag(L)).sum()
    return mu, np.sqrt(var), lml


def bayes_opt(space: Space, objective, budget: int, seed: int = 0, n_init: int = 3) -> Trace:
    """GP (RBF kernel, length scale chosen by marginal likelihood) + expected improvement,
    maximised over the valid discrete candidate set (or a 2000-sample pool)."""
    rng, tr = random.Random(seed), Trace("bayes_gp_ei")
    valid = []
    if space.size() <= 5000:
        for cfg in space.all():
            r = space.validate(cfg)
            (tr.invalid.append((cfg, r)) if r else valid.append(cfg))
    else:
        while len(valid) < 2000:
            cfg = space.sample(rng)
            if not space.validate(cfg):
                valid.append(cfg)
    rng.shuffle(valid)
    done = set()
    for cfg in valid[:n_init]:
        _eval(tr, objective, cfg)
        done.add(key(cfg))
    while len(tr.evals) < budget and len(done) < len(valid):
        t0 = time.perf_counter()
        X = np.array([space.encode(c) for c, _, _ in tr.evals])
        y = np.array([v for _, v, _ in tr.evals])
        ym, ys = y.mean(), y.std() or 1.0
        yn = (y - ym) / ys
        cands = [c for c in valid if key(c) not in done]
        Xs = np.array([space.encode(c) for c in cands])
        best_ls, best_lml = 0.5, -np.inf
        for ls in (0.2, 0.35, 0.5, 0.8, 1.2):
            try:
                _, _, lml = _gp_posterior(X, yn, X[:1], ls)
            except np.linalg.LinAlgError:
                continue
            if lml > best_lml:
                best_ls, best_lml = ls, lml
        mu, sd, _ = _gp_posterior(X, yn, Xs, best_ls)
        fbest = yn.min()
        z = (fbest - mu - 0.01) / sd
        ei = (fbest - mu - 0.01) * norm.cdf(z) + sd * norm.pdf(z)
        nxt = cands[int(np.argmax(ei))]
        tr.overhead_s += time.perf_counter() - t0
        _eval(tr, objective, nxt)
        done.add(key(nxt))
    return tr
