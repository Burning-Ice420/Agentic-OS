# Provenance schema v1

The kernel emits one line per event on COM1 (the serial log). User-space tools in
`hivemind/services/provenance` parse it. Values never leave the kernel in events:
only their length and a 32-bit FNV-1a hash.

## Wire format

```
PEV|1|<runtime_uuid>|<event_id>|<tick>|<kind>|<actor>|<mem_id>|<mem_name>|<key>|<parent|->|<attrs>
PSTAT|1|<runtime_uuid>|emitted=..;dropped=..;drained=..;pending=..;next_id=..
SSTAT|1|<runtime_uuid>|policy=..;budget=..;ticks=..;fired=..;deferred=..;max_wait_ticks=..
BENCH|1|<runtime_uuid>|<name>|n=..;cycles_per_op=..;ns_per_op=..;tsc_per_pit=..
```

| Field | Meaning |
|---|---|
| `runtime_uuid` | per-boot instance UUID (`sysinfo`). Global event id = `runtime_uuid:event_id` |
| `event_id` | per-instance monotonic counter. Gaps = events dropped (ring full) |
| `tick` | PIT tick (≈18.2 Hz). Monotonic per boot. **No wall-clock time**: the RTC is second-granular and not used per event |
| `kind` | see below |
| `actor` | `kernel` `shell` `desktop` `restore` `peer` `agent:<id>` `model:agent:<id>` `model:shell` |
| `mem_id`, `mem_name`, `key` | target; `0`/`-` when none; `*` = whole node |
| `parent` | event id of the cause on the same instance, or `-` when the kernel does not know it |
| `attrs` | `k=v;k=v`. `len`, `vh` (value hash), `auth` (allow event that authorized a write), `op`, `cap`, `reason`, `pv` (policy version), `rule`, `cond`, `tool`, `prompt_len`, `ctx_len`, … |

`|`, `;` and newlines inside names/keys are replaced by `_`.

## Event kinds

| Kind | Emitted when | Parent |
|---|---|---|
| `create` | node created | – |
| `write` | blob written (any path) | causal event if known |
| `read` | an agent's rule matched (the observation that made it fire), an offload gathered context (`*`), a shell `blob read` / `ai` | firing event for context reads |
| `agent_fire` | rule fired on a false→true edge | the `read` |
| `offload` | node state sent to the model device | the context `read` |
| `mesh_send` | value broadcast to peer VMs | the local `write` |
| `link`, `page_out`, `page_in` | graph edge added, tiered-memory moves | – |
| `restore` | one summary per disk restore (`nodes`, `blobs`, `prior_lineage=unknown`) | – |
| `cap_allow` / `cap_deny` | capability decision at the enforcement point | causal event if known |
| `cap_change` | enforce on/off, grant, delegate, revoke | – |
| `agent_spawned`, `agent_rule` | agent lifecycle | – |
| `marker` | `prov mark <label>`: delimits a workflow | – |

## How parents are known (and when they are not)

* **Explicit** (kernel knows the cause): write ← agent_fire ← read; mesh_send ← write;
  offload ← context read ← agent_fire; model write ← offload.
* **Model answers** are matched to the oldest outstanding request for the same node
  (FIFO). The bridge answers in order, so this is exact for one in-flight request per
  node and a documented inference otherwise.
* **Peer writes** carry no parent (the mesh wire has no sender id). The collector
  links one to a `mesh_send` on another instance **only if exactly one** sender has
  the same node, key and value hash, and labels that link `inferred`. Zero or several
  candidates → `unknown` / `ambiguous`; nothing is fabricated.
* **Not recorded:** periodic rule evaluations that did not fire (they would flood the
  ring at every tick); reads by the kernel itself; pre-reboot lineage of restored blobs.

## Loss

The ring holds 512 events between drains. When full, the **new** event is dropped,
its id is still consumed (so the collector sees a gap) and the kernel writes a
`PSTAT` line with the cumulative drop count. Loss is therefore measurable two ways.

## Mapping to the blueprint envelope

`Event.to_envelope()` returns: `schema_version`, `event_id` (`runtime:id`),
`agent_id`, `actor`, `event_type`, `monotonic_tick`, `parent_event_ids`,
`resource_id` (`mem:<id>/<key>`), `source_id` (node name), `attributes`,
`runtime_id`. `timestamp_utc` is null: the kernel has no per-event wall clock.

## Reconstructed links (user space)
* `read_from`: a read is linked to the latest earlier write of the same blob on the same
  instance, confirmed by the value hash; a mismatch is reported as `reads_inconsistent`,
  never linked. A whole-node read (`*`, offload context) links to the latest write of
  every key in the node.
* `taint <event>`: walks explicit, inferred and read-from links and reports every
  ancestor write by an untrusted actor (default `peer`) or namespace prefix, with a path.
* Offload events carry `device=absent` when no model accelerator was attached.
