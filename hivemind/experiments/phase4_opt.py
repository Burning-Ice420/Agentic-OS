"""Phase 4: runtime configuration search.

  sched  (simulated) tune scheduler config on two workload classes; exhaustive grid
         gives the true optimum, so random vs Bayesian search is scored by regret.
         Transfer: optimum found on workload A, evaluated on workload B.
  ai     (measured) tune the AI-bridge config (model, num_predict, temperature,
         prompt) for accuracy under a hard latency ceiling, with real model calls.

  python experiments/phase4_opt.py sched
  python experiments/phase4_opt.py ai --budget 6
"""
from __future__ import annotations

import argparse
import importlib.util
import os
import statistics
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.abspath(os.path.join(HERE, "..")))
from experiments import calllog, record  # noqa: E402
from services.optimizer.search import bayes_opt, grid_search, random_search  # noqa: E402
from services.optimizer.space import Param, Space, key  # noqa: E402
from services.scheduler.metrics import summarize  # noqa: E402
from services.scheduler.sim import simulate  # noqa: E402
from services.scheduler.workload import generate  # noqa: E402

SLOT_CEILING = 3


def sched_space():
    s = Space("scheduler", [Param("policy", ["fifo", "priority", "edf", "dep_aware", "slack_shed"]),
                            Param("model_slots", [1, 2, 3, 4]),
                            Param("retries", [0, 1, 2]),
                            Param("backoff", [0.25, 0.5, 1.0, 2.0])])
    s.constraints.append(lambda c: f"model_slots>{SLOT_CEILING} exceeds resource ceiling"
                         if c["model_slots"] > SLOT_CEILING else None)
    return s


def make_objective(workload: str):
    cache = {}
    if workload == "A_medium_chain":
        mix, rate = ("chain", "chain", "single"), 0.35
    else:
        mix, rate = ("fanout", "fanout", "single"), 0.30

    def obj(cfg):
        k = key(cfg)
        if k in cache:
            return cache[k]
        misses = []
        for s in range(3):
            ws = generate(seed=500 + s, n=150, rate=rate, shape_mix=mix)
            for w in ws:
                for t in w.tasks.values():
                    t.max_retries = cfg["retries"]
            r = simulate(ws, {"model": cfg["model_slots"], "cpu": 4}, cfg["policy"],
                         fail_prob=0.05, backoff=cfg["backoff"], seed=s)
            misses.append(summarize(r)["deadline_miss_rate"])
        v = statistics.fmean(misses) + 0.05 * cfg["model_slots"]   # deadline misses + resource cost
        cache[k] = v
        return v
    return obj


def sched(seeds=10, budget=15):
    space = sched_space()
    out, failed = {}, []
    for wl in ("A_medium_chain", "B_overload_fanout"):
        obj = make_objective(wl)
        t0 = time.perf_counter()
        grid = grid_search(space, obj, budget=10_000)
        grid_s = time.perf_counter() - t0
        opt = grid.best()
        res = {"grid": {"optimum": opt[1], "config": opt[0], "evaluations": len(grid.evals),
                        "invalid_rejected": len(grid.invalid), "wall_s": grid_s}}
        for name, fn in (("random", random_search), ("bayes_gp_ei", bayes_opt)):
            regrets, to5, inval, ovh = [], [], [], []
            for s in range(seeds):
                tr = fn(space, obj, budget, seed=s)
                regrets.append(tr.best()[1] - opt[1])
                c = tr.curve()
                hit = next((i + 1 for i, v in enumerate(c) if v - opt[1] <= 0.02), None)
                to5.append(hit if hit else budget + 1)
                inval.append(len(tr.invalid))
                ovh.append(tr.overhead_s)
            res[name] = {"regret_mean": statistics.fmean(regrets), "regret_sd": statistics.stdev(regrets),
                         "evals_to_within_0.02_mean": statistics.fmean(to5),
                         "found_within_0.02": sum(1 for x in to5 if x <= budget),
                         "invalid_proposals_mean": statistics.fmean(inval),
                         "optimizer_overhead_s_mean": statistics.fmean(ovh)}
        out[wl] = res
        print(wl, "optimum", round(opt[1], 4), opt[0])
        for m in ("random", "bayes_gp_ei"):
            r = res[m]
            print(f"   {m:<12} regret {r['regret_mean']:.4f}±{r['regret_sd']:.4f}  "
                  f"hit-within-0.02 {r['found_within_0.02']}/{seeds}  evals-to-hit {r['evals_to_within_0.02_mean']:.1f}  "
                  f"invalid {r['invalid_proposals_mean']:.1f}")
    # transfer: A's optimum on B
    a_cfg = out["A_medium_chain"]["grid"]["config"]
    b_obj = make_objective("B_overload_fanout")
    transfer = {"config_from_A": a_cfg, "value_on_B": b_obj(a_cfg),
                "B_optimum": out["B_overload_fanout"]["grid"]["optimum"]}
    transfer["transfer_regret"] = transfer["value_on_B"] - transfer["B_optimum"]
    print("transfer A->B:", transfer)
    metrics = {"results": {"label": "simulated", "value": out}, "transfer": {"label": "simulated", "value": transfer}}
    path = record.write("phase4_sched", {"space": [(p.name, p.values) for p in space.params],
                                         "ceiling_model_slots": SLOT_CEILING, "budget": budget,
                                         "objective": "mean deadline miss rate over 3 workload seeds + 0.05*model_slots"},
                        metrics, seed="search seeds 0..9; workload seeds 500..502", repetitions=seeds,
                        failed_runs=failed)
    print("record:", path)


# ── measured: AI bridge configuration ─────────────────────────────────────────

FEWSHOT = ("You are a decision engine inside an OS kernel. Reply with ONLY one line key=value, "
           "a short snake_case key and a one-word value. Examples: state temp=30 task overheating? -> "
           "overheating=false ; state log=disk failure task error or info? -> log_level=error ; "
           "state action=delete everything task run it? -> run=false")


def ai(budget=6, latency_ceiling=4.0):
    root = os.path.join(HERE, "..", "hivemind-os")
    sb = importlib.util.spec_from_file_location("bridge", os.path.join(root, "hive-llm-bridge.py"))
    bridge = importlib.util.module_from_spec(sb); sb.loader.exec_module(bridge)
    sa = importlib.util.spec_from_file_location("aib", os.path.join(root, "ai-bench.py"))
    aib = importlib.util.module_from_spec(sa); sa.loader.exec_module(aib)

    space = Space("ai_bridge", [Param("model", ["llama3.2:1b", "llama3.2:3b"]),
                                Param("num_predict", [8, 16, 32]),
                                Param("temperature", [0.0, 0.3]),
                                Param("prompt", ["strict", "fewshot"])])
    cache, log = {}, []

    def obj(cfg):
        k = key(cfg)
        if k in cache:
            return cache[k][0]
        bridge.call_ollama("http://localhost:11434", cfg["model"], "warm", "say ok", "")  # warm-up, not timed
        correct, lats = 0, []
        for t in aib.TASKS:
            s = time.perf_counter()
            try:
                kk, vv = bridge.call_ollama("http://localhost:11434", cfg["model"], t["name"], t["prompt"], t["ctx"],
                                            options={"num_predict": cfg["num_predict"], "temperature": cfg["temperature"]},
                                            system=FEWSHOT if cfg["prompt"] == "fewshot" else None)
            except Exception as e:  # noqa: BLE001
                kk, vv = "error", type(e).__name__
            lats.append(time.perf_counter() - s)
            correct += aib.scored(t, kk, vv)
            calllog.log(source="phase4_ai", model=cfg["model"], num_predict=cfg["num_predict"],
                        temperature=cfg["temperature"], fewshot=cfg["prompt"] == "fewshot",
                        prompt_len=len(t["prompt"]), ctx_len=len(t["ctx"]), out_len=len(f"{kk}={vv}"),
                        latency_s=lats[-1], task=t["name"])
        acc = correct / len(aib.TASKS)
        lat = statistics.fmean(lats)
        infeasible = lat > latency_ceiling
        v = -acc + (1.0 if infeasible else 0.0)
        cache[k] = (v, acc, lat)
        log.append({"config": cfg, "accuracy": acc, "mean_latency_s": lat, "feasible": not infeasible})
        print(f"   eval {cfg} -> acc {acc:.2f} lat {lat:.2f}s {'INFEASIBLE' if infeasible else ''}", flush=True)
        return v

    res = {}
    for name, fn in (("random", random_search), ("bayes_gp_ei", bayes_opt)):
        tr = fn(space, obj, budget, seed=0)
        b = tr.best()
        res[name] = {"best_config": b[0], "best_accuracy": cache[key(b[0])][1],
                     "best_latency_s": cache[key(b[0])][2], "curve_neg_acc": tr.curve(),
                     "evaluations": len(tr.evals), "optimizer_overhead_s": tr.overhead_s}
        print(name, "best", res[name]["best_config"], "acc", res[name]["best_accuracy"])
    res["grid"] = {"feasible": space.size() <= budget,
                   "note": f"space has {space.size()} configs x 13 real model calls; exceeds budget {budget}"}
    metrics = {"search": {"label": "measured", "value": res},
               "all_evaluations": {"label": "measured", "value": log},
               "distinct_configs_evaluated": {"label": "measured", "value": len(cache)}}
    path = record.write("phase4_ai", {"space": [(p.name, p.values) for p in space.params],
                                      "latency_ceiling_s": latency_ceiling, "budget": budget,
                                      "objective": "-accuracy (13 tasks, polarity scorer) + 1 if mean latency > ceiling"},
                        metrics, seed=0, repetitions=1, warmup="one untimed call per config",
                        notes="single run per config; accuracy granularity is 1/13; cache shares evals across methods")
    print("record:", path)


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("mode", choices=["sched", "ai"])
    ap.add_argument("--budget", type=int, default=6)
    a = ap.parse_args()
    sched() if a.mode == "sched" else ai(budget=a.budget)
