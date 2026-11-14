"""Live end-to-end check of provenance (Phase 1) and capability enforcement
(Phase 2) on a real guest. Ground truth is the scripted workflow below; every
check compares the reconstructed lineage / decisions to it.

  python experiments/phase12_live.py [--boot C:/hivemind/boot_p.bin] [--attempts 3]
"""
from __future__ import annotations

import argparse
import os
import shutil
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.abspath(os.path.join(HERE, "..")))
from experiments import record  # noqa: E402
from experiments.guest import Guest, kill_all_qemu  # noqa: E402
from services.provenance import Lineage  # noqa: E402
from services.provenance.graph import format_chain  # noqa: E402

WORKFLOW = [
    # (command, seconds to wait afterwards)
    ("prov mark wf1_lineage", 0.6),
    ("blob write 1 temp 50", 0.8),
    ("agent new watcher 1", 0.6),              # agent 2
    ("agent rule 2 temp gt:80 alert high", 0.6),
    ("blob write 1 temp 95", 2.8),            # agent 2 fires: read -> fire -> write -> mesh_send
    ("prov mark wf2_capabilities", 0.6),
    ("cap enforce on", 0.6),
    ("agent new rogue 1", 0.6),                # agent 3
    ("agent rule 3 temp gt:90 hijack yes", 0.6),
    ("agent rule 2 temp gt:85 decision ai:check", 0.6),
    ("blob write 1 temp 50", 2.8),
    ("blob write 1 temp 97", 2.8),            # both agents: read denied
    ("cap grant 2 node:1 rw", 0.6),
    ("blob write 1 temp 50", 2.8),
    ("blob write 1 temp 98", 2.8),            # agent 2: read+write allowed, ai tool denied; agent 3 denied
    ("cap list", 0.8),
    ("prov flush", 1.5),
    ("prov stats", 1.0),
]


def run_once(boot: str, name: str):
    kill_all_qemu()
    g = Guest(name, boot).start()
    if not g.wait_ready():
        g.stop()
        return None, "guest did not reach prompt within 55s"
    t0 = time.time()
    for c, wait in WORKFLOW:
        g.type(c, after=wait)
    elapsed = time.time() - t0
    screen = g.screen()
    g.stop()
    return (g.read_log(), screen, elapsed), None


def check(lines: list[str]) -> dict:
    g = Lineage.from_lines(lines)
    ev = list(g.events.values())
    checks = {}

    alert_w = [e for e in ev if e.kind == "write" and e.key == "alert" and e.actor == "agent:2"]
    first = min(alert_w, key=lambda e: e.id) if alert_w else None
    chain = g.chain(first.gid) if first else []
    kinds = [s["event"].kind for s in chain if "event" in s]
    checks["agent_write_has_attributed_writer"] = bool(first)
    checks["agent_write_chain_is_write<-fire<-read(temp)"] = kinds == ["write", "agent_fire", "read"] and \
        chain[-1]["event"].key == "temp"
    checks["agent_write_was_broadcast_with_parent"] = any(
        e.kind == "mesh_send" and first and e.parent == first.id for e in ev)

    wf2 = g.workflow("wf2_capabilities")
    deny = [e for e in wf2 if e.kind == "cap_deny"]
    allow = [e for e in wf2 if e.kind == "cap_allow"]
    checks["rogue_agent_3_denied"] = any(e.actor == "agent:3" for e in deny)
    checks["rogue_agent_3_never_wrote"] = not any(e.kind == "write" and e.actor == "agent:3" for e in ev)
    checks["agent_2_denied_before_grant"] = any(e.actor == "agent:2" and e.attrs.get("op") == "r" for e in deny)
    checks["agent_2_allowed_after_grant_read_and_write"] = {e.attrs.get("op") for e in allow if e.actor == "agent:2"} >= {"r", "w"}
    authed = [e for e in wf2 if e.kind == "write" and e.actor == "agent:2" and "auth" in e.attrs]
    checks["authorized_write_cites_its_allow_event"] = bool(authed) and all(
        g.events.get(f"{e.runtime}:{e.attrs['auth']}") is not None and
        g.events[f"{e.runtime}:{e.attrs['auth']}"].kind == "cap_allow" for e in authed)
    checks["ai_tool_denied_without_capability"] = any(
        e.actor == "agent:2" and e.attrs.get("op") == "i" for e in deny)
    st = g.stats()
    rt = st["instances"][0] if st["instances"] else None
    dropped = int(st["kernel_reported"].get(rt, {}).get("dropped", -1)) if rt else -1
    checks["no_events_dropped_kernel_report"] = dropped == 0
    checks["no_id_gaps"] = all(v == 0 for v in st["id_gaps"].values())
    checks["no_malformed_lines"] = st["malformed_lines"] == 0
    checks["every_write_has_actor"] = st["writes"] == st["writes_with_actor"]
    checks["every_agent_or_model_write_has_parent"] = st["agent_or_model_writes"] == st["agent_or_model_writes_with_parent"]
    explain = g.explain(deny[0].gid) if deny else None
    return {"checks": checks, "stats": st, "chain_text": format_chain(g, chain) if chain else "",
            "explain_example": explain}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--boot", default=r"C:\hivemind\boot_p.bin")
    ap.add_argument("--attempts", type=int, default=3)
    a = ap.parse_args()

    failed = []
    result = None
    for i in range(a.attempts):
        out, err = run_once(a.boot, f"p12_{i}")
        if err:
            failed.append({"attempt": i, "reason": err})
            print(f"attempt {i}: FAILED ({err})")
            continue
        result = out
        break
    if result is None:
        path = record.write("phase12_live", {"boot": a.boot}, {}, failed_runs=failed, notes="no successful boot")
        print("no successful run; record:", path)
        return 1

    lines, screen, elapsed = result
    fixture = os.path.join(HERE, "..", "services", "tests", "fixtures", "live_phase12.log")
    os.makedirs(os.path.dirname(fixture), exist_ok=True)
    with open(fixture, "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")
    rep = check(lines)
    for k, v in rep["checks"].items():
        print(f"  {'PASS' if v else 'FAIL'}  {k}")
    print("\nlineage of agent 2's first alert write:\n" + rep["chain_text"])
    print("\nexplain first denial:", rep["explain_example"])
    print("\nstats:", {k: rep["stats"][k] for k in ("events", "writes", "reads", "id_gaps", "malformed_lines")})
    metrics = {
        "checks": {"label": "measured", "value": rep["checks"]},
        "all_pass": {"label": "measured", "value": all(rep["checks"].values())},
        "event_counts": {"label": "measured", "value": rep["stats"]["by_kind"]},
        "kernel_reported": {"label": "measured", "value": rep["stats"]["kernel_reported"]},
        "workflow_seconds": {"label": "measured", "value": round(elapsed, 1)},
    }
    path = record.write("phase12_live", {"boot": a.boot, "workflow": WORKFLOW}, metrics,
                        failed_runs=failed, notes="fixture: services/tests/fixtures/live_phase12.log")
    print("\nrecord:", path)
    print("ALL PASS" if all(rep["checks"].values()) else "SOME CHECKS FAILED")
    return 0 if all(rep["checks"].values()) else 2


if __name__ == "__main__":
    sys.exit(main())
