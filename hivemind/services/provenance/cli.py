"""Provenance CLI.

  python -m services.provenance.cli --log serial.log stats
  python -m services.provenance.cli --log a.log --log b.log trace blob shared/temp
  python -m services.provenance.cli --log serial.log trace event 42
  python -m services.provenance.cli --log serial.log trace workflow demo1
  python -m services.provenance.cli --log serial.log explain 57
  python -m services.provenance.cli --log serial.log export out.jsonl
"""
from __future__ import annotations

import argparse
import json
import sys

from .graph import Lineage, format_chain


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(prog="hivemind-prov")
    ap.add_argument("--log", action="append", required=True, help="kernel serial log(s) with PEV lines")
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("stats")
    t = sub.add_parser("trace")
    t.add_argument("what", choices=["blob", "event", "workflow"])
    t.add_argument("ref")
    t.add_argument("--runtime")
    e = sub.add_parser("explain")
    e.add_argument("ref")
    tt = sub.add_parser("taint", help="did untrusted content reach this event?")
    tt.add_argument("ref")
    tt.add_argument("--untrusted-actor", action="append", default=None)
    tt.add_argument("--untrusted-prefix", action="append", default=[])
    x = sub.add_parser("export")
    x.add_argument("out")
    a = ap.parse_args(argv)

    g = Lineage.from_files(a.log)

    if a.cmd == "stats":
        print(json.dumps(g.stats(), indent=2))
        return 0
    if a.cmd == "export":
        with open(a.out, "w", encoding="utf-8") as f:
            for ev in sorted(g.events.values(), key=lambda e: (e.runtime, e.id)):
                f.write(json.dumps(ev.to_envelope()) + "\n")
        print(f"wrote {len(g.events)} envelopes to {a.out}")
        return 0
    if a.cmd == "taint":
        gid = g.resolve(a.ref)
        if not gid:
            print(f"event {a.ref} not found")
            return 1
        print(json.dumps(g.taint(gid, tuple(a.untrusted_actor or ["peer"]), tuple(a.untrusted_prefix)), indent=2))
        return 0
    if a.cmd == "explain":
        gid = g.resolve(a.ref)
        if not gid:
            print(f"event {a.ref} not found (or ambiguous across instances; use runtime:id)")
            return 1
        print(json.dumps(g.explain(gid), indent=2))
        return 0

    if a.what == "blob":
        if "/" not in a.ref:
            print("use MEMORY/KEY")
            return 2
        mem, key = a.ref.split("/", 1)
        gid = g.latest_write(mem, key, a.runtime)
        if not gid:
            print(f"no recorded write to {a.ref}")
            return 1
        print(format_chain(g, g.chain(gid)))
    elif a.what == "event":
        gid = g.resolve(a.ref)
        if not gid:
            print(f"event {a.ref} not found")
            return 1
        print(format_chain(g, g.chain(gid)))
    else:
        evs = g.workflow(a.ref)
        if not evs:
            print(f"no marker '{a.ref}'")
            return 1
        for ev in evs:
            tgt = f"{ev.mem_name}/{ev.key}" if ev.mem_id else ev.key
            par = "-" if ev.parent is None else ev.parent
            print(f"{ev.gid}  {ev.kind:<12} {ev.actor:<16} {tgt:<28} parent={par}  "
                  + ";".join(f"{k}={v}" for k, v in ev.attrs.items()))
    return 0


if __name__ == "__main__":
    sys.exit(main())
