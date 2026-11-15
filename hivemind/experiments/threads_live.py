"""Life threads (kernel routing, focus, private threads) on a real guest with the real 3B model.

Boot 1 (fresh data disk):
  three threads (work, home, health); health made private
  task add +1 call acme about the invoice      -> work, by keyword
  task add +1 buy milk #home                   -> home, by tag
  note refill meds on friday                   -> health (private): tagging refused, nothing sent
  note water the plants on sunday              -> no rule matches: the model routes it, kernel validates
  note team meeting moved to thursday          -> work
  ask what is happening this week              -> the meds note is kept back by the kernel
  a mesh peer relabels the meds note as work, and tries to make health public
  ask again                                    -> still kept back
  focus work, wait for both reminders          -> work reminder interrupts, home reminder held
  focus off                                    -> digest with the held reminder
Boot 2 (same disk): threads, privacy and routing survived the reboot.

  python experiments/threads_live.py
"""
from __future__ import annotations

import os
import re
import socket
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.abspath(os.path.join(HERE, "..")))
from experiments import record  # noqa: E402
from experiments.guest import Guest, kill_all_qemu  # noqa: E402
from services.provenance import Lineage  # noqa: E402

BOOT = r"C:\hivemind\boot_p.bin"
PEER, LLM = 4476, 4477
MODEL = "llama3.2:3b"
WORK = r"C:\hivemind"
SECRET = "refill meds"


def start(name, keep):
    g = Guest(name, BOOT, com2=f"tcp:127.0.0.1:{PEER},server,nowait",
              com3=f"tcp:127.0.0.1:{LLM},server,nowait", data_disk_mb=4, keep_data=keep).start()
    time.sleep(2)
    peer = socket.create_connection(("127.0.0.1", PEER), timeout=5)
    blog = os.path.join(WORK, f"{name}-bridge.txt")
    br = subprocess.Popen([sys.executable, "-u", os.path.join(HERE, "..", "hivemind-os", "hive-llm-bridge.py"),
                           "--port", str(LLM), "--model", MODEL], stdout=open(blog, "w"), stderr=subprocess.STDOUT)
    return g, peer, br, blog


def main():
    kill_all_qemu()
    shots = {}

    def snap(g, label):
        shots[label] = g.screen()
        return shots[label]

    # ── boot 1 ──
    g, peer, br, blog1 = start("threads", keep=False)
    if not g.wait_ready():
        print("boot 1 did not reach the prompt"); g.stop(); br.kill(); return 1
    g.type("thread add work meeting email acme standup", after=1)
    g.type("thread add home milk rent laundry groceries", after=1)
    g.type("thread add health doctor meds gym", after=1)
    g.type("thread private health", after=1)
    g.type("clear", after=0.5)
    t_task = time.time()
    g.type("task add +1 call acme about the invoice", after=1.5)
    g.type("task add +1 buy milk #home", after=1.5)
    g.type("focus work", after=1)
    snap(g, "1_tasks_routed")
    g.type("clear", after=0.5)
    g.type("note refill meds on friday", after=3)
    g.type("note water the plants on sunday", after=14)
    g.type("note team meeting moved to thursday", after=8)
    snap(g, "1_notes_routed")
    g.type("clear", after=0.5)
    g.type("ask what is happening this week", after=14)
    snap(g, "1_ask")
    # hostile peer: relabel the private note, and try to make the private thread public
    peer.sendall(b"HMSG|notes|n1_thread|work\n")
    peer.sendall(b"HMSG|threads|health|public|doctor meds gym\n")
    time.sleep(2)
    g.type("clear", after=0.5)
    g.type("ask what is happening this week", after=14)
    g.type("thread", after=2)
    snap(g, "1_after_peer")
    # wait for both reminders (due at the next minute + 1)
    while time.time() - t_task < 140:
        time.sleep(5)
    snap(g, "1_reminders")
    g.type("clear", after=0.5)
    g.type("focus", after=1.5)
    snap(g, "1_focus_held")
    g.type("clear", after=0.5)
    g.type("focus off", after=2)
    snap(g, "1_digest")
    g.type("clear", after=0.5)
    g.type("why tasks t1_thread", after=2)
    snap(g, "1_why_t1")
    g.type("clear", after=0.5)
    g.type("why notes n2_thread", after=2)
    snap(g, "1_why_n2")
    g.type("save", after=4)
    g.type("prov flush", after=2)
    g.stop(); peer.close(); br.kill()
    log1 = g.read_log()
    bridge1 = open(blog1, encoding="utf-8", errors="replace").read()

    # ── boot 2 ──
    time.sleep(2)
    g, peer, br, blog2 = start("threads", keep=True)
    if not g.wait_ready():
        print("boot 2 did not reach the prompt"); g.stop(); br.kill(); return 1
    g.type("thread", after=2)
    snap(g, "2_threads")
    g.type("clear", after=0.5)
    g.type("note list", after=2)
    snap(g, "2_notes")
    g.type("clear", after=0.5)
    g.type("ask when do i refill meds", after=6)
    snap(g, "2_ask")
    g.type("ui tasks", after=3)
    snap(g, "3_desktop_tasks")
    for _ in range(5):
        g.cmd("sendkey esc"); time.sleep(0.4)
    g.type("prov flush", after=2)
    g.stop(); peer.close(); br.kill()
    log2 = g.read_log()
    bridge2 = open(blog2, encoding="utf-8", errors="replace").read()

    for k, v in shots.items():
        with open(os.path.join(WORK, f"threads_{k}.txt"), "w", encoding="utf-8") as f:
            f.write(v)

    L1, L2 = Lineage.from_lines(log1), Lineage.from_lines(log2)
    ev1, ev2 = list(L1.events.values()), list(L2.events.values())

    def routes(evs, node, key):
        return [e for e in evs if e.kind == "marker" and e.attrs.get("route") == "1"
                and e.key == key and e.mem_name == node]

    def final_label(evs, node, key):
        w = [e for e in evs if e.kind == "write" and e.mem_name == node and e.key == key + "_thread"
             and e.actor == "kernel"]
        r = routes(evs, node, key)
        return r[-1].attrs.get("thread") if r else None

    reqs = re.findall(r"REQ  (\w+): task='(.*?)' state='(.*?)'", bridge1 + bridge2)
    n2_routes = routes(ev1, "notes", "n2")
    n2_final = n2_routes[-1] if n2_routes else None
    held = [e for e in ev1 if e.kind == "marker" and e.attrs.get("held") == "1"]
    deny_private = [e for e in ev1 + ev2 if e.kind == "cap_deny" and e.attrs.get("reason") == "private_thread"]
    asks = [e for e in ev1 + ev2 if e.kind == "read" and e.mem_name == "notes" and "private_withheld" in e.attrs
            and "n1" not in e.attrs.get("sent", "").split("+")]
    checks = {
        "task_routed_by_keyword": any(e.attrs.get("thread") == "work" and e.attrs.get("reason") == "keyword"
                                      for e in routes(ev1, "tasks", "t1")),
        "task_routed_by_tag": any(e.attrs.get("thread") == "home" and e.attrs.get("reason") == "tag"
                                  for e in routes(ev1, "tasks", "t2")),
        "private_note_routed_to_health": final_label(ev1, "notes", "n1") in ("health",) or
                                         any(e.attrs.get("thread") == "health" for e in routes(ev1, "notes", "n1")),
        "private_note_tagging_refused": len(deny_private) >= 1,
        "unmatched_note_routed_by_model_and_validated": n2_final is not None
                                                        and n2_final.attrs.get("reason") in ("model", "model_rejected")
                                                        and n2_final.attrs.get("thread") in ("work", "home", "health", "inbox"),
        "work_note_routed": any(e.attrs.get("thread") == "work" for e in routes(ev1, "notes", "n3")),
        "ask_kept_private_note_back": len(asks) >= 2,
        "secret_never_sent_to_model": all(SECRET not in st.lower() for _, _, st in reqs),
        "peer_relabel_ignored": "PRIVATE" in shots["1_after_peer"] and len(asks) >= 2,
        "focus_held_home_reminder": any(e.key == "t2" and e.attrs.get("thread") == "home" for e in held),
        "focus_let_work_reminder_through": not any(e.key == "t1" for e in held)
                                           and any(e.kind == "write" and e.mem_name == "tasks" and e.key == "t1"
                                                   and e.actor == "kernel" for e in ev1),
        "peer_cannot_edit_threads": any(e.kind == "cap_deny" and e.attrs.get("reason") == "protected_node"
                                        and e.actor == "peer" for e in ev1),
        "digest_lists_held_reminder": "milk" in shots["1_digest"] and "held" in shots["1_digest"],
        "why_explains_routing": "keyword: acme" in shots["1_why_t1"],
        "threads_survived_reboot": "PRIVATE" in shots["2_threads"] and "work" in shots["2_threads"],
        "private_after_reboot": any(e.kind == "read" and e.mem_name == "notes" and "private_withheld" in e.attrs
                                    and "n1" not in e.attrs.get("sent", "").split("+") for e in ev2)
                                and all(SECRET not in st.lower() for _, _, st in re.findall(
                                    r"REQ  (\w+): task='(.*?)' state='(.*?)'", bridge2)),
        "desktop_shows_threads": "work" in shots["3_desktop_tasks"] and "home" in shots["3_desktop_tasks"],
        "no_event_loss": all(v == 0 for v in L1.id_gaps().values()) and all(v == 0 for v in L2.id_gaps().values()),
    }
    for k, v in checks.items():
        print(f"  {'PASS' if v else 'FAIL'}  {k}")
    print("\nmodel requests:")
    for r in reqs:
        print("  ", r)
    print("n2 routing:", [(e.attrs.get("thread"), e.attrs.get("reason")) for e in n2_routes])
    fx = os.path.join(HERE, "..", "services", "tests", "fixtures")
    for tag, lines in (("1", log1), ("2", log2)):
        with open(os.path.join(fx, f"live_threads_boot{tag}.log"), "w", encoding="utf-8") as f:
            f.write("\n".join(lines) + "\n")
    path = record.write("threads_live", {"model": MODEL},
                        {"checks": {"label": "measured", "value": checks},
                         "requests": {"label": "measured", "value": reqs},
                         "n2_routing": {"label": "measured", "value": [dict(e.attrs) for e in n2_routes]},
                         "screens": {"label": "measured", "value": shots}},
                        failed_runs=[], repetitions=1,
                        notes="fixtures: services/tests/fixtures/live_threads_boot1.log, live_threads_boot2.log")
    print("record:", path)
    print("ALL PASS" if all(checks.values()) else "SOME CHECKS FAILED")
    return 0 if all(checks.values()) else 2


if __name__ == "__main__":
    sys.exit(main())
