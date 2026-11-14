"""Two real guests on one mesh: does lineage survive crossing machines?

  A: shell writes plant/temp=95 -> watcher agent (temp gt:80) writes alert=high -> mesh
  hub (this script) relays HMSG lines A <-> B, timestamping each one on the host
  B: peer write plant/alert -> responder agent (alert eq:high) writes action=shutdown -> mesh -> A

Checks, on the merged PEV logs of both guests:
  * B's peer write of `alert` is linked (inferred, exactly one candidate) to A's mesh_send
  * the chain from B's `action` write reaches A's shell write of `temp`
  * A's peer write of `action` is linked back to B's mesh_send
Propagation time is measured at the hub (host clock), so it includes guest serial I/O.

  python experiments/two_vm_mesh.py
"""
from __future__ import annotations

import os
import re
import socket
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.abspath(os.path.join(HERE, "..")))
from experiments import record  # noqa: E402
from experiments.guest import Guest, kill_all_qemu  # noqa: E402
from services.provenance import Lineage  # noqa: E402
from services.provenance.graph import format_chain  # noqa: E402

BOOT = r"C:\hivemind\boot_p.bin"
PA, PB = 4471, 4472


class Hub:
    def __init__(self):
        self.a = socket.create_connection(("127.0.0.1", PA), timeout=5)
        self.b = socket.create_connection(("127.0.0.1", PB), timeout=5)
        self.relayed = []          # (host_t, direction, line)
        self.stop = False
        for src, dst, tag in ((self.a, self.b, "A->B"), (self.b, self.a, "B->A")):
            threading.Thread(target=self._pump, args=(src, dst, tag), daemon=True).start()

    def _pump(self, src, dst, tag):
        src.settimeout(0.5)
        buf = b""
        while not self.stop:
            try:
                d = src.recv(4096)
            except socket.timeout:
                continue
            except OSError:
                return
            if not d:
                return
            buf += d
            while b"\n" in buf:
                ln, buf = buf.split(b"\n", 1)
                self.relayed.append((time.perf_counter(), tag, ln.decode(errors="replace").strip()))
                try:
                    dst.sendall(ln + b"\n")
                except OSError:
                    return

    def close(self):
        self.stop = True
        for s in (self.a, self.b):
            try:
                s.close()
            except OSError:
                pass


def node_id(screen, name):
    for ln in screen.splitlines():
        m = re.match(rf"^\s*(\d+)\s+{re.escape(name)}\b", ln)
        if m:
            return int(m.group(1))
    return None


def agent_id(g, name, node):
    g.type(f"agent new {name} {node}", after=0.8)
    ids = re.findall(rf"Agent '{name}' created -> id #(\d+)", g.screen())
    return int(ids[-1]) if ids else None


def run_once(attempt):
    kill_all_qemu()
    A = Guest(f"meshA_{attempt}", BOOT, monitor_port=5590, com2=f"tcp:127.0.0.1:{PA},server,nowait").start()
    time.sleep(20)                                     # stagger boots (WHPX contention)
    B = Guest(f"meshB_{attempt}", BOOT, monitor_port=5591, com2=f"tcp:127.0.0.1:{PB},server,nowait").start()
    time.sleep(2)
    hub = Hub()
    try:
        if not B.wait_ready() or "hive>" not in A.screen():
            return None, "a guest did not reach the prompt"
        A.type("mem new plant", after=0.8)
        A.type("mem", after=1.0)
        na = node_id(A.screen(), "plant")
        A.type("net send plant seed 0", after=2.0)     # creates `plant` on B as a peer node
        B.type("mem", after=1.0)
        nb = node_id(B.screen(), "plant")
        if not na or not nb:
            return None, f"node ids not found (A={na}, B={nb})"
        resp = agent_id(B, "responder", nb)
        if not resp:
            return None, "responder id not found"
        B.type(f"agent rule {resp} alert eq:high action shutdown", after=0.5)
        watch = agent_id(A, "watcher", na)
        if not watch:
            return None, "watcher id not found"
        A.type(f"agent rule {watch} temp gt:80 alert high", after=0.5)
        A.type("prov mark mesh_demo", after=0.3)
        B.type("prov mark mesh_demo", after=0.3)
        t_write = time.perf_counter()
        A.type(f"blob write {na} temp 95", after=8.0)
        for g in (A, B):
            g.type("prov flush", after=1.5)
        return {"A": A, "B": B, "hub": hub, "t_write_cmd_end": t_write,
                "ids": {"node_A": na, "node_B": nb, "watcher_A": watch, "responder_B": resp}}, None
    finally:
        hub.close()
        A.stop(); B.stop()


def main():
    failed, run = [], None
    for attempt in range(3):
        run, why = run_once(attempt)
        if run:
            break
        failed.append({"attempt": attempt, "reason": why})
        print("attempt failed:", why)
    if not run:
        record.write("two_vm_mesh", {}, {}, failed_runs=failed)
        return 1
    la, lb = run["A"].read_log(), run["B"].read_log()
    fx = os.path.join(HERE, "..", "services", "tests", "fixtures")
    for tag, lines in (("A", la), ("B", lb)):
        with open(os.path.join(fx, f"live_mesh_{tag}.log"), "w", encoding="utf-8") as f:
            f.write("\n".join(lines) + "\n")
    L = Lineage.from_lines(la + lb)
    st = L.stats()
    evs = list(L.events.values())
    rt_a = next((e.runtime for e in evs if e.kind == "write" and e.actor == "shell" and e.key == "temp"), None)
    b_alert = [e for e in evs if e.runtime != rt_a and e.kind == "write" and e.actor == "peer" and e.key == "alert"]
    b_action = [e for e in evs if e.runtime != rt_a and e.kind == "write" and e.key == "action" and e.actor.startswith("agent")]
    a_action = [e for e in evs if e.runtime == rt_a and e.kind == "write" and e.actor == "peer" and e.key == "action"]
    chain_kinds, chain_txt = [], ""
    if b_action:
        ch = L.chain(b_action[0].gid)
        chain_kinds = [(s["event"].runtime == rt_a, s["event"].kind, s["event"].actor, s["link"]) for s in ch if "event" in s]
        chain_txt = format_chain(L, ch)
    checks = {
        "two_instances_in_log": len(st["instances"]) == 2,
        "B_received_alert_as_peer": bool(b_alert),
        "B_alert_inferred_from_A_mesh_send": bool(b_alert) and b_alert[0].gid in L.inferred,
        "B_responder_acted": bool(b_action),
        "B_action_chain_reaches_A_shell_write": any(onA and k == "write" and a == "shell" for onA, k, a, _ in chain_kinds),
        "A_received_action_back": bool(a_action),
        "A_action_inferred_from_B_mesh_send": bool(a_action) and a_action[0].gid in L.inferred,
        "no_drops_or_gaps": all(v == 0 for v in st["id_gaps"].values()),
        "no_ambiguous_links": st["peer_writes_ambiguous"] == 0,
    }
    for k, v in checks.items():
        print(f"  {'PASS' if v else 'FAIL'}  {k}")
    print("\nchain of B's action, across both machines:\n" + chain_txt)
    hub = run["hub"].relayed
    t_alert = next((t for t, d, ln in hub if d == "A->B" and "|alert|" in ln), None)
    t_action = next((t for t, d, ln in hub if d == "B->A" and "|action|" in ln), None)
    rtt = (t_action - t_alert) if (t_alert and t_action) else None
    if rtt is not None:
        print(f"\nhub saw A's alert, then B's reaction {rtt * 1000:.0f} ms later (host clock, one run)")
    metrics = {"checks": {"label": "measured", "value": checks},
               "relayed_lines": {"label": "measured", "value": [(d, ln) for _, d, ln in hub]},
               "b_reaction_after_alert_at_hub_ms": {"label": "measured", "value": rtt and rtt * 1000},
               "lineage_stats": {"label": "measured", "value": st}}
    path = record.write("two_vm_mesh", {"ids": run["ids"], "accel": "whpx", "hub": "python TCP relay"},
                        metrics, failed_runs=failed, repetitions=1,
                        notes="fixtures: services/tests/fixtures/live_mesh_A.log, live_mesh_B.log; "
                              "reaction time is one sample, includes serial I/O and the hub")
    print("record:", path)
    print("ALL PASS" if all(checks.values()) else "SOME CHECKS FAILED")
    return 0 if all(checks.values()) else 2


if __name__ == "__main__":
    sys.exit(main())
