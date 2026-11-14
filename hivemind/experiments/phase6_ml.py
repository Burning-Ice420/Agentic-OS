"""Phase 6 (optional advanced ML): learned task-duration prediction for the scheduler.

Question: does a learned model predict a model call's latency better than simple
baselines, from features known BEFORE the call? Data: every real call logged by
Phases 4 and 5 (experiments/results/model_calls.jsonl).

Models
  global_mean     one constant
  per_model_mean  mean latency per model (what the scheduler effectively uses now)
  linear          least squares on [1, is_3b, num_predict, num_predict*is_3b,
                  prompt_len, ctx_len, fewshot, temperature]
Excluded on purpose: output length (only known after the call).

Evaluation: 5-fold CV (seeded), MAE and RMSE, paired per fold.

  python experiments/phase6_ml.py
"""
from __future__ import annotations

import json
import os
import statistics
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.abspath(os.path.join(HERE, "..")))
from experiments import record  # noqa: E402

DATA = os.path.join(HERE, "results", "model_calls.jsonl")


def feats(r):
    b = 1.0 if "3b" in r["model"] else 0.0
    return [1.0, b, r["num_predict"], r["num_predict"] * b, r["prompt_len"], r["ctx_len"],
            1.0 if r.get("fewshot") else 0.0, r.get("temperature", 0.1)]


def main():
    rows = [json.loads(l) for l in open(DATA, encoding="utf-8") if l.strip()]
    rows = [r for r in rows if 0 < r["latency_s"] < 120]
    n = len(rows)
    if n < 40:
        print(f"only {n} calls logged; not enough data for a meaningful comparison")
        record.write("phase6_ml", {}, {"n_calls": {"label": "measured", "value": n}},
                     notes="insufficient data; not run")
        return
    rng = np.random.default_rng(0)
    idx = rng.permutation(n)
    folds = np.array_split(idx, 5)
    X = np.array([feats(r) for r in rows])
    y = np.array([r["latency_s"] for r in rows])
    models = [r["model"] for r in rows]
    res = {m: {"mae": [], "rmse": []} for m in ("global_mean", "per_model_mean", "linear")}
    for k in range(5):
        te = folds[k]
        tr = np.concatenate([folds[j] for j in range(5) if j != k])
        preds = {
            "global_mean": np.full(len(te), y[tr].mean()),
            "per_model_mean": np.array([y[[i for i in tr if models[i] == models[t]]].mean()
                                        if any(models[i] == models[t] for i in tr) else y[tr].mean() for t in te]),
        }
        w, *_ = np.linalg.lstsq(X[tr], y[tr], rcond=None)
        preds["linear"] = X[te] @ w
        for m, p in preds.items():
            err = p - y[te]
            res[m]["mae"].append(float(np.abs(err).mean()))
            res[m]["rmse"].append(float(np.sqrt((err ** 2).mean())))
    summary = {m: {k: {"mean": statistics.fmean(v), "sd": statistics.stdev(v)} for k, v in d.items()}
               for m, d in res.items()}
    wins = sum(1 for a, b in zip(res["linear"]["mae"], res["per_model_mean"]["mae"]) if a < b)
    print(f"{n} real model calls; latency mean {y.mean():.2f}s sd {y.std(ddof=1):.2f}s")
    for m, s in summary.items():
        print(f"  {m:<15} MAE {s['mae']['mean']:.3f} ± {s['mae']['sd']:.3f} s   RMSE {s['rmse']['mean']:.3f} s")
    print(f"  linear beats per_model_mean on {wins}/5 folds")
    metrics = {"n_calls": {"label": "measured", "value": n},
               "cv": {"label": "derived", "value": summary},
               "linear_beats_per_model_mean_folds": {"label": "derived", "value": wins}}
    path = record.write("phase6_ml", {"features": "1,is_3b,num_predict,num_predict*is_3b,prompt_len,ctx_len,fewshot,temperature",
                                      "folds": 5}, metrics, seed=0, repetitions=5,
                        notes="per-call latencies from phases 4 and 5; out_len excluded (leakage)")
    print("record:", path)


if __name__ == "__main__":
    main()
