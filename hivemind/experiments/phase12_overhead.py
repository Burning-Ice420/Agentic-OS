"""Overhead of provenance and capability enforcement, measured inside the guest
with the TSC (calibrated against the PIT). Runs in its own boot because the
benchmark deliberately discards events (which would look like drops in a trace).

  python experiments/phase12_overhead.py [--boot C:/hivemind/boot_p.bin] [--reps 3]
"""
from __future__ import annotations

import argparse
import os
import statistics
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.abspath(os.path.join(HERE, "..")))
from experiments import record  # noqa: E402
from experiments.guest import Guest, kill_all_qemu  # noqa: E402


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--boot", default=r"C:\hivemind\boot_p.bin")
    ap.add_argument("--reps", type=int, default=3)
    ap.add_argument("--attempts", type=int, default=3)
    a = ap.parse_args()
    failed = []
    lines = None
    for i in range(a.attempts):
        kill_all_qemu()
        g = Guest(f"ovh_{i}", a.boot).start()
        if not g.wait_ready():
            failed.append({"attempt": i, "reason": "no prompt within 55s"})
            g.stop()
            continue
        for _ in range(a.reps):
            g.type("bench prov 2000", after=8.0)
        for c in (1, 16, 64, 256):
            for _ in range(a.reps):
                g.type(f"bench cap 20000 {c}", after=4.0)
        g.stop()
        lines = g.read_log()
        break
    if lines is None:
        print("no successful boot", failed)
        record.write("phase12_overhead", {"boot": a.boot}, {}, failed_runs=failed)
        return 1
    rows = {}
    for ln in lines:
        if ln.startswith("BENCH|"):
            _, _, _, name, attrs = ln.strip().split("|", 4)
            kv = dict(x.split("=") for x in attrs.split(";"))
            rows.setdefault(name, []).append({k: int(v) for k, v in kv.items()})
    summary = {}
    for name, rs in rows.items():
        ns = [r["ns_per_op"] for r in rs]
        cyc = [r["cycles_per_op"] for r in rs]
        summary[name] = {"ns_mean": statistics.fmean(ns), "ns_sd": statistics.stdev(ns) if len(ns) > 1 else 0.0,
                         "cycles_mean": statistics.fmean(cyc), "runs": len(rs)}
        print(f"{name:<28} {summary[name]['ns_mean']:9.0f} ± {summary[name]['ns_sd']:6.0f} ns/op  "
              f"({summary[name]['cycles_mean']:.0f} cycles, n={len(rs)})")
    off = summary.get("write.prov_off", {}).get("ns_mean")
    if off:
        for k in ("write.prov_on_record", "write.prov_on_ship_com1"):
            if k in summary:
                summary[k]["overhead_vs_off_x"] = summary[k]["ns_mean"] / off
                print(f"  {k}: {summary[k]['overhead_vs_off_x']:.2f}x the cost of a write without provenance")
    path = record.write("phase12_overhead", {"boot": a.boot, "writes_per_run": 2000, "checks_per_run": 20000,
                                             "cap_table_sizes": [1, 16, 64, 256], "accel": "whpx"},
                        {"bench": {"label": "measured", "value": summary}}, repetitions=a.reps,
                        failed_runs=failed, notes="TSC in a WHPX guest, calibrated vs PIT; virtualized timing")
    print("record:", path)


if __name__ == "__main__":
    sys.exit(main())
