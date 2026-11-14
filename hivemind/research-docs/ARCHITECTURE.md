# Architecture after the research integration

Rule followed throughout: **the kernel owns mechanisms at the enforcement point;
user-space services own policy analytics, search and measurement.**

```
 bare-metal guest (hivemind-os, no_std)                     host user space (Python)
 ────────────────────────────────────────                   ─────────────────────────────────
 memory graph (hive/)                                       services/provenance  collector, lineage
   write_blob_as(actor,…) ── authorize() ── cap_core        graph, trace/explain CLI
   note_read(), create_memory_as()            (enforce)     services/scheduler   DAG scheduler,
 agents (agent/)  edge-triggered rules                       policies, simulator, real executor
   sched_core: order + per-tick budget                       services/optimizer   validator +
 prov_core ring ─ drain ─► COM1  PEV|… PSTAT|… SSTAT|…       random / grid / GP-EI search
 llm (COM3)  offload ↔ answer linked by FIFO                 services/energy      RAPL via EMI,
 net (COM2)  peer writes checked as `peer`                   idle baseline, J/success
 bench.rs    TSC micro-benchmarks ─► BENCH|…                 experiments/          harnesses + records
```

## Kernel additions (and why each must be in the kernel)
| Addition | File | Why kernel |
|---|---|---|
| `Actor` on every write, `write_blob_as` | `hive/mod.rs` | attribution is only trustworthy at the point the write happens |
| capability checks | `hive/mod.rs`, `cap_core.rs` | enforcement must happen at the resource boundary, not in the agent |
| provenance ring + drain | `prov_core.rs`, `prov.rs` | only the kernel knows the real cause of each write |
| scheduling order + budget | `agent/mod.rs`, `sched_core.rs` | agents run inside the kernel tick |
| overhead benchmark | `bench.rs` | measures the kernel paths themselves |
| COM3 presence probe | `llm.rs` | without it an offload to a missing device spins with interrupts off |

Pure logic (`prov_core`, `cap_core`, `sched_core`) uses only `core` + `alloc` and is
unit-tested on the host by the `hivemind-os-logic` crate, which compiles the kernel's
own source files.

## Not in the kernel (by design)
Lineage queries, inference of cross-instance and read-from links, taint queries
(did an untrusted write reach this action?), policy explanation from traces,
DAG scheduling with deadlines, configuration search, energy accounting.

## Backward compatibility
`write_blob`/`create_memory` keep their signatures (attributed to `kernel`); wire
formats HMSG and LLMREQ/LLMRSP are unchanged; capability enforcement is off unless
enabled; provenance lines on COM1 are ignored by older tools.
