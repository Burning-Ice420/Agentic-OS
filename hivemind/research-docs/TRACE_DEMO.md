# Trace demo, on a real recorded run

Everything below is output of the provenance CLI on
`services/tests/fixtures/live_phase12.log`, a serial log captured from one real guest
boot (QEMU + WHPX) by `experiments/phase12_live.py`. Nothing here is hand written.

```
cd hivemind
F=services/tests/fixtures/live_phase12.log
```

## 1. Is the log complete?

```
python -m services.provenance.cli --log $F stats
```
Key fields from the run: 38 events, 0 malformed lines, 0 id gaps, and the kernel's own
counters agree (`emitted 38, dropped 0, drained 38`). 10 of 10 writes carry an actor,
2 of 2 agent writes carry a parent, 3 of 3 reads link to the write they observed.

## 2. Why does `alert` have this value?

```
python -m services.provenance.cli --log $F trace blob kernel-root/alert
```
```
:32  write        by agent:2   kernel-root/alert  len=4;vh=3dddb9fd;auth=31
  <- [explicit]  :30  agent_fire  by agent:2   kernel-root/alert  rule=0
    <- [explicit]  :29  read      by agent:2   kernel-root/temp   len=2;vh=13fea564;rule=0;cond=>80
      <- [read_from] :27  write   by shell     kernel-root/temp   len=2;vh=13fea564
         origin: root
```
(runtime uuid shortened). Read bottom up: the shell wrote `temp` (27), agent 2 read
that exact value (29, same value hash), its rule `>80` fired (30), and it wrote
`alert` (32). `auth=31` names the capability decision that allowed the write.

Link labels are part of the answer. `explicit` was recorded by the kernel at the time.
`read_from` is reconstructed: the latest earlier write to the same blob on the same
instance, which is exact under the single hive lock and is cross-checked with the value
hash. A cross-instance link would print `inferred`, and an unlinkable one says so.

## 3. Why was this agent refused?

```
python -m services.provenance.cli --log $F explain 17
```
```json
{ "decision": "DENY", "requester": "agent:3", "operation": "r",
  "target": "kernel-root/temp", "matched_capability": null,
  "deny_reason": "no_capability", "policy_version": "2",
  "policy_changes_before": ["enforce_on@14"] }
```
Agent 3 ("rogue") never received a capability, so with enforcement on it is denied and
never writes. Event 36 shows agent 2 itself being denied `tool:ai` because it was
granted node access but not the tool.

## 4. What happened in one workflow?

```
python -m services.provenance.cli --log $F trace workflow wf2_capabilities
```
Lists every event from the `prov mark wf2_capabilities` marker until the next marker:
enforcement switched on, the rogue agent spawned and denied, agent 2 denied before its
grant and allowed after it.

## 5. Did untrusted content reach this action?

```
python -m services.provenance.cli --log $F taint 38
```
returns `"tainted": false` for the final mesh send, because every ancestor write came
from the shell. The injection experiment (`experiments/mvp_injection.py`, see
RESULTS.md) runs the same query where a mesh peer did write the source content.

## What this does not show
* Values are never logged, only length and a 32-bit FNV hash, so the trace proves which
  write was observed, not what the text said.
* `read_from` assumes no events were lost. If the ring dropped events the id gaps show
  it and a hash mismatch is reported as `reads_inconsistent` rather than linked.
* The trace covers what the kernel mediates. Anything a model does outside the guest
  (inside the Ollama bridge) is outside the graph.
