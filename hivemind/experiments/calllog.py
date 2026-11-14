"""Append one JSON line per real model call (dataset for Phase 6)."""
import json
import os
import time

PATH = os.path.join(os.path.dirname(__file__), "results", "model_calls.jsonl")


def log(**row):
    os.makedirs(os.path.dirname(PATH), exist_ok=True)
    row["t"] = time.time()
    with open(PATH, "a", encoding="utf-8") as f:
        f.write(json.dumps(row) + "\n")
