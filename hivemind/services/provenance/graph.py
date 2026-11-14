"""Lineage graph over kernel provenance events.

Link types
  explicit  the kernel recorded the parent id itself (it knew the cause).
  read_from a read is linked to the write it observed: the latest earlier write to the
            same blob on the same instance. Exact under the single-lock hive (writes
            and event ids are totally ordered); confirmed by the value hash, and
            reported as inconsistent if the hashes differ (e.g. events were lost).
            A whole-node read (`*`) links to the latest write of every key in it.
  inferred  cross-instance: a peer write on instance B is matched to exactly one
            `mesh_send` on another instance with the same node name, key and value
            hash. Never asserted when zero or several candidates exist.

Origins (when a chain has no further parent)
  root      initiated by a trusted principal (shell, desktop, kernel)
  unknown   no recorded or inferable cause (e.g. peer write with no matching send)
  restore   restored from disk; pre-reboot lineage is not known
  missing   the recorded parent id is absent from the log (dropped or truncated)
"""
from __future__ import annotations

from collections import defaultdict
from typing import Iterable, Optional

from .schema import Event, ParseError, parse_line

TRUSTED_ROOTS = ("shell", "desktop", "kernel")


class Lineage:
    def __init__(self):
        self.events: dict[str, Event] = {}
        self.pstat: dict[str, dict] = {}
        self.sstat: dict[str, dict] = {}
        self.malformed: list[tuple[int, str]] = []
        self.inferred: dict[str, str] = {}            # child gid -> parent gid
        self.ambiguous: dict[str, int] = {}           # child gid -> #candidates
        self.read_from: dict[str, list[str]] = {}     # read gid -> write gids observed
        self.read_inconsistent: list[str] = []
        self._seq = 0
        self._finalized = False

    # ── loading ──────────────────────────────────────────────────────────────

    def add_line(self, line: str):
        self._seq += 1
        try:
            r = parse_line(line, self._seq)
        except (ParseError, ValueError) as e:
            self.malformed.append((self._seq, f"{e}: {line.strip()[:120]}"))
            return
        if r is None:
            return
        if r[0] == "event":
            ev = r[1]
            self.events[ev.gid] = ev
        elif r[0] == "pstat":
            self.pstat[r[1]] = r[2]
        else:
            self.sstat[r[1]] = r[2]
        self._finalized = False

    @classmethod
    def from_lines(cls, lines: Iterable[str]) -> "Lineage":
        g = cls()
        for ln in lines:
            g.add_line(ln)
        g.finalize()
        return g

    @classmethod
    def from_files(cls, paths: Iterable[str]) -> "Lineage":
        g = cls()
        for p in paths:
            with open(p, encoding="utf-8", errors="replace") as f:
                for ln in f:
                    g.add_line(ln)
        g.finalize()
        return g

    def finalize(self):
        """Compute inferred cross-instance links."""
        self.inferred.clear()
        self.ambiguous.clear()
        sends = defaultdict(list)
        for e in self.events.values():
            if e.kind == "mesh_send":
                sends[(e.mem_name, e.key, e.attrs.get("vh"))].append(e)
        for e in self.events.values():
            if e.kind == "write" and e.actor == "peer" and e.parent is None:
                cands = [s for s in sends.get((e.mem_name, e.key, e.attrs.get("vh")), [])
                         if s.runtime != e.runtime]
                if len(cands) == 1:
                    self.inferred[e.gid] = cands[0].gid
                elif len(cands) > 1:
                    self.ambiguous[e.gid] = len(cands)
        # read-from links
        self.read_from.clear()
        self.read_inconsistent.clear()
        writes = defaultdict(list)   # (runtime, mem_id, key) -> [write events]
        for e in self.events.values():
            if e.kind == "write":
                writes[(e.runtime, e.mem_id, e.key)].append(e)
        for lst in writes.values():
            lst.sort(key=lambda e: e.id)
        by_node = defaultdict(list)
        for (rt, mid, key) in writes:
            by_node[(rt, mid)].append(key)
        for e in self.events.values():
            if e.kind != "read" or not e.mem_id:
                continue
            keys = by_node[(e.runtime, e.mem_id)] if e.key == "*" else [e.key]
            found = []
            for k in keys:
                prior = [w for w in writes.get((e.runtime, e.mem_id, k), []) if w.id < e.id]
                if not prior:
                    continue
                w = prior[-1]
                if e.key != "*" and "vh" in e.attrs and w.attrs.get("vh") != e.attrs.get("vh"):
                    self.read_inconsistent.append(e.gid)
                    continue
                found.append(w.gid)
            if found:
                self.read_from[e.gid] = found
        self._finalized = True

    # ── queries ──────────────────────────────────────────────────────────────

    def resolve(self, ref: str) -> Optional[str]:
        """Accept 'runtime:id' or a bare id when only one runtime matches."""
        if ref in self.events:
            return ref
        if ref.isdigit():
            hits = [g for g, e in self.events.items() if e.id == int(ref)]
            if len(hits) == 1:
                return hits[0]
        return None

    def parent_of(self, gid: str):
        e = self.events[gid]
        if e.parent is not None:
            return (e.parent_gid, "explicit")
        if gid in self.inferred:
            return (self.inferred[gid], "inferred")
        return (None, None)

    def all_parents(self, gid: str) -> list:
        """Every known cause of an event: explicit parent, inferred cross-instance
        sender, and the writes a read observed."""
        out = []
        p, kind = self.parent_of(gid)
        if p:
            out.append((p, kind))
        for w in self.read_from.get(gid, []):
            out.append((w, "read_from"))
        return out

    def ancestors(self, gid: str, limit: int = 10_000) -> dict:
        """All ancestors reachable through any link, with the link used to reach each."""
        seen, frontier = {}, [gid]
        while frontier and len(seen) < limit:
            cur = frontier.pop()
            for p, kind in self.all_parents(cur):
                if p not in seen and p in self.events:
                    seen[p] = (cur, kind)
                    frontier.append(p)
        return seen

    def taint(self, gid: str, untrusted_actors=("peer",), untrusted_prefixes=()) -> dict:
        """Did content from an untrusted origin reach this event? Returns the
        untrusted ancestor writes and one path from each back to `gid`."""
        anc = self.ancestors(gid)
        hits = []
        for a in anc:
            e = self.events[a]
            if e.kind == "write" and (e.actor in untrusted_actors or
                                      any(e.mem_name.startswith(p) for p in untrusted_prefixes)):
                path, cur = [a], a
                while cur != gid and cur in anc:
                    cur = anc[cur][0]
                    path.append(cur)
                hits.append({"untrusted_write": a, "actor": e.actor, "target": f"{e.mem_name}/{e.key}",
                             "path": path})
        return {"event": gid, "tainted": bool(hits), "sources": hits, "ancestors": len(anc)}

    def origin(self, e: Event) -> str:
        if e.parent is not None and e.parent_gid not in self.events:
            return "missing"
        if e.kind == "restore" or e.actor == "restore":
            return "restore"
        if e.actor in TRUSTED_ROOTS:
            return "root"
        return "unknown"

    def chain(self, gid: str, max_depth: int = 64) -> list[dict]:
        """Walk parents from `gid` back to an origin. Each step records how it was linked."""
        steps, seen, cur, link = [], set(), gid, "start"
        while cur and cur not in seen and len(steps) < max_depth:
            seen.add(cur)
            e = self.events.get(cur)
            if e is None:
                steps.append({"gid": cur, "link": link, "missing": True})
                break
            steps.append({"gid": cur, "link": link, "event": e})
            nxt, link = self.parent_of(cur)
            if nxt is None and len(self.read_from.get(cur, [])) == 1:
                nxt, link = self.read_from[cur][0], "read_from"
            if nxt is None:
                if len(self.read_from.get(cur, [])) > 1:
                    steps[-1]["reads_from"] = list(self.read_from[cur])
                steps[-1]["origin"] = self.origin(e)
                if cur in self.ambiguous:
                    steps[-1]["ambiguous_candidates"] = self.ambiguous[cur]
                break
            cur = nxt
        return steps

    def latest_write(self, mem_name: str, key: str, runtime: Optional[str] = None) -> Optional[str]:
        ws = [e for e in self.events.values()
              if e.kind == "write" and e.mem_name == mem_name and e.key == key
              and (runtime is None or e.runtime == runtime)]
        if not ws:
            return None
        return max(ws, key=lambda e: (e.seq, e.id)).gid

    def workflow(self, label: str) -> list[Event]:
        """Events from marker `label` up to the next marker on the same instance."""
        marks = sorted((e for e in self.events.values() if e.kind == "marker"), key=lambda e: (e.runtime, e.id))
        out = []
        for m in marks:
            if m.key != label:
                continue
            nxt = [x.id for x in marks if x.runtime == m.runtime and x.id > m.id]
            end = min(nxt) if nxt else float("inf")
            out += sorted((e for e in self.events.values()
                           if e.runtime == m.runtime and m.id <= e.id < end), key=lambda e: e.id)
        return out

    def explain(self, gid: str) -> dict:
        e = self.events[gid]
        if e.kind not in ("cap_allow", "cap_deny"):
            return {"event": gid, "note": f"{e.kind} is not a policy decision"}
        changes = sorted((c for c in self.events.values()
                          if c.runtime == e.runtime and c.kind == "cap_change" and c.id < e.id),
                         key=lambda c: c.id)
        used_by = [w.gid for w in self.events.values()
                   if w.runtime == e.runtime and w.attrs.get("auth") == str(e.id)]
        return {
            "event": gid,
            "decision": "ALLOW" if e.kind == "cap_allow" else "DENY",
            "requester": e.actor,
            "operation": e.attrs.get("op"),
            "target": f"{e.mem_name}" + (f"/{e.key}" if e.key not in ("-", "") else ""),
            "matched_capability": e.attrs.get("cap"),
            "deny_reason": e.attrs.get("reason"),
            "policy_version": e.attrs.get("pv"),
            "policy_changes_before": [f"{c.key}@{c.id}" for c in changes],
            "authorized_events": used_by,
        }

    def id_gaps(self) -> dict[str, int]:
        """Missing event ids per instance between its first and last observed id."""
        by = defaultdict(list)
        for e in self.events.values():
            by[e.runtime].append(e.id)
        return {rt: (max(ids) - min(ids) + 1 - len(ids)) for rt, ids in by.items()}

    def stats(self) -> dict:
        kinds = defaultdict(int)
        for e in self.events.values():
            kinds[e.kind] += 1
        writes = [e for e in self.events.values() if e.kind == "write"]
        reads = [e for e in self.events.values() if e.kind == "read"]
        agent_writes = [w for w in writes if w.actor.startswith(("agent:", "model:agent:"))]
        peer_writes = [w for w in writes if w.actor == "peer"]
        return {
            "instances": sorted({e.runtime for e in self.events.values()}),
            "events": len(self.events),
            "by_kind": dict(sorted(kinds.items())),
            "writes": len(writes),
            "writes_with_actor": sum(1 for w in writes if w.actor),
            "agent_or_model_writes": len(agent_writes),
            "agent_or_model_writes_with_parent": sum(1 for w in agent_writes if w.parent is not None),
            "reads": len(reads),
            "peer_writes": len(peer_writes),
            "peer_writes_inferred": sum(1 for w in peer_writes if w.gid in self.inferred),
            "peer_writes_ambiguous": sum(1 for w in peer_writes if w.gid in self.ambiguous),
            "peer_writes_unknown": sum(1 for w in peer_writes
                                       if w.gid not in self.inferred and w.gid not in self.ambiguous),
            "reads_linked_to_writes": len(self.read_from),
            "reads_inconsistent": len(self.read_inconsistent),
            "dangling_parents": sum(1 for e in self.events.values()
                                    if e.parent is not None and e.parent_gid not in self.events),
            "id_gaps": self.id_gaps(),
            "kernel_reported": self.pstat,
            "malformed_lines": len(self.malformed),
        }


def format_chain(g: Lineage, steps: list[dict]) -> str:
    out = []
    for i, s in enumerate(steps):
        pad = "  " * i
        if s.get("missing"):
            out.append(f"{pad}<- [{s['link']}] {s['gid']}  (NOT IN LOG: dropped or truncated)")
            continue
        e = s["event"]
        tgt = f"{e.mem_name}/{e.key}" if e.mem_id else e.key
        extra = ";".join(f"{k}={v}" for k, v in e.attrs.items())
        arrow = "" if i == 0 else f"<- [{s['link']}] "
        out.append(f"{pad}{arrow}{e.gid}  {e.kind:<12} by {e.actor:<16} {tgt}  {extra}")
        if "origin" in s:
            amb = f" ({s['ambiguous_candidates']} candidate senders, not linked)" if "ambiguous_candidates" in s else ""
            out.append(f"{pad}   origin: {s['origin']}{amb}")
        if "reads_from" in s:
            out.append(f"{pad}   whole-node read observed {len(s['reads_from'])} writes (use `taint` to search them)")
    return "\n".join(out)
