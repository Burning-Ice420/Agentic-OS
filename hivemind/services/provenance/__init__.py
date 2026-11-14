"""Provenance service: parse the kernel's PEV event stream, build a lineage graph,
and answer trace / explain queries. User space only; the kernel just emits events.

Schema: research-docs/PROVENANCE_SCHEMA.md
"""
from .schema import parse_line, Event, ParseError  # noqa: F401
from .graph import Lineage  # noqa: F401
