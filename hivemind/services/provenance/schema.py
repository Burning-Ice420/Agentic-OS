"""Schema v1 parser for kernel provenance lines.

Kernel wire format (one line per event, COM1):
    PEV|1|<runtime_uuid>|<id>|<tick>|<kind>|<actor>|<mem_id>|<mem_name>|<key>|<parent|->|<attrs>
Plus two status line types:
    PSTAT|1|<runtime_uuid>|emitted=..;dropped=..;drained=..;pending=..;next_id=..
    SSTAT|1|<runtime_uuid>|policy=..;budget=..;ticks=..;fired=..;deferred=..;max_wait_ticks=..

`to_envelope()` maps an event onto the blueprint's common event envelope.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Optional

SCHEMA_VERSION = 1
KINDS = {
    "create", "write", "read", "link", "agent_fire", "offload", "mesh_send", "page_out",
    "page_in", "restore", "cap_allow", "cap_deny", "cap_change", "agent_spawned",
    "agent_rule", "marker",
}


class ParseError(ValueError):
    pass


def parse_attrs(s: str) -> dict:
    out = {}
    if not s:
        return out
    for part in s.split(";"):
        if not part:
            continue
        if "=" not in part:
            raise ParseError(f"bad attribute {part!r}")
        k, v = part.split("=", 1)
        out[k] = v
    return out


@dataclass(frozen=True)
class Event:
    runtime: str
    id: int
    tick: int
    kind: str
    actor: str
    mem_id: int
    mem_name: str
    key: str
    parent: Optional[int]
    attrs: dict = field(default_factory=dict, hash=False, compare=False)
    seq: int = 0  # order of arrival in the log (for inference tie-breaks)

    @property
    def gid(self) -> str:
        """Globally unique event id across instances."""
        return f"{self.runtime}:{self.id}"

    @property
    def parent_gid(self) -> Optional[str]:
        return f"{self.runtime}:{self.parent}" if self.parent is not None else None

    def to_envelope(self) -> dict:
        agent = None
        if self.actor.startswith("agent:"):
            agent = self.actor
        elif self.actor.startswith("model:agent:"):
            agent = self.actor[len("model:"):]
        return {
            "schema_version": "1.0",
            "event_id": self.gid,
            "workflow_id": None,
            "task_id": None,
            "agent_id": agent,
            "actor": self.actor,
            "event_type": self.kind,
            "timestamp_utc": None,          # kernel has tick time only (see schema doc)
            "monotonic_tick": self.tick,
            "parent_event_ids": [self.parent_gid] if self.parent_gid else [],
            "resource_id": f"mem:{self.mem_id}/{self.key}" if self.mem_id else None,
            "source_id": self.mem_name,
            "attributes": dict(self.attrs),
            "runtime_id": self.runtime,
        }


def parse_line(line: str, seq: int = 0):
    """Return ('event', Event) | ('pstat', runtime, dict) | ('sstat', runtime, dict) | None."""
    line = line.strip()
    if line.startswith("PEV|"):
        parts = line.split("|")
        if len(parts) != 12:
            raise ParseError(f"expected 12 fields, got {len(parts)}")
        _, ver, runtime, eid, tick, kind, actor, mem_id, mem_name, key, parent, attrs = parts
        if int(ver) != SCHEMA_VERSION:
            raise ParseError(f"unsupported schema version {ver}")
        if kind not in KINDS:
            raise ParseError(f"unknown kind {kind!r}")
        if not runtime:
            raise ParseError("empty runtime id")
        return ("event", Event(runtime=runtime, id=int(eid), tick=int(tick), kind=kind, actor=actor,
                               mem_id=int(mem_id), mem_name=mem_name, key=key,
                               parent=None if parent == "-" else int(parent),
                               attrs=parse_attrs(attrs), seq=seq))
    if line.startswith("PSTAT|") or line.startswith("SSTAT|"):
        parts = line.split("|")
        if len(parts) != 4:
            raise ParseError("bad status line")
        tag = "pstat" if line.startswith("PSTAT|") else "sstat"
        return (tag, parts[2], parse_attrs(parts[3]))
    return None
