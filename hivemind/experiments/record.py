"""Experiment record: every result file carries what is needed to reproduce it
(blueprint section 5.5): commit, dirty flag, OS, CPU, tool versions, config,
seed, repetitions, warm-up policy, metrics, and failed runs with reasons.

Each metric must carry a label: measured | derived | simulated | estimated | proposed.
"""
from __future__ import annotations

import datetime
import json
import os
import platform
import subprocess

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
RESULTS = os.path.join(os.path.dirname(__file__), "results")
QEMU = r"C:\msys64\mingw64\bin\qemu-system-x86_64.exe"

LABELS = {"measured", "derived", "simulated", "estimated", "proposed"}


def _run(cmd):
    try:
        return subprocess.run(cmd, capture_output=True, text=True, timeout=20, cwd=ROOT).stdout.strip()
    except Exception as e:  # noqa: BLE001
        return f"<unavailable: {type(e).__name__}>"


def environment() -> dict:
    cpu = _run(["powershell", "-NoProfile", "-Command", "(Get-CimInstance Win32_Processor).Name"])
    ram = _run(["powershell", "-NoProfile", "-Command",
                "[math]::Round((Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory/1GB,1)"])
    return {
        "git_commit": _run(["git", "rev-parse", "HEAD"]),
        "git_dirty": bool(_run(["git", "status", "--porcelain", "--untracked-files=no"])),
        "os": platform.platform(),
        "cpu": cpu,
        "ram_gb": ram,
        "python": platform.python_version(),
        "qemu": _run([QEMU, "--version"]).splitlines()[0] if os.path.exists(QEMU) else None,
        "rustc_nightly": _run(["rustc", "+nightly", "--version"]),
        "ollama": _run(["ollama", "--version"]),
        "utc": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
    }


def write(name: str, config: dict, metrics: dict, *, seed=None, repetitions=1, warmup="none",
          failed_runs=None, notes="") -> str:
    for k, v in metrics.items():
        if isinstance(v, dict) and "label" in v and v["label"] not in LABELS:
            raise ValueError(f"metric {k} has invalid label {v['label']}")
    os.makedirs(RESULTS, exist_ok=True)
    stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    path = os.path.join(RESULTS, f"{name}-{stamp}.json")
    rec = {
        "experiment": name,
        "environment": environment(),
        "config": config,
        "seed": seed,
        "repetitions": repetitions,
        "warmup": warmup,
        "failed_runs": failed_runs or [],
        "metrics": metrics,
        "notes": notes,
    }
    with open(path, "w", encoding="utf-8") as f:
        json.dump(rec, f, indent=2)
    return path
