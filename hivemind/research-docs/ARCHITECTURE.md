# Architecture (updated 2026-11-15)

For the why behind it, read OVERVIEW.md first.

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
   model_context(): taint policy + private threads ─► COM3
 apps/, threads/  tasks, notes, ask, why, focus, routing
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
| taint bit per value, carried through agents and model answers | `hive/mod.rs` (`write_blob_tx`), `agent/mod.rs`, `llm.rs` | trust has to follow data across every app; only the write path sees all of it |
| taint policy (off / block / redact) | `apps_core.rs` (`build_context`), `Hive::model_context` | the only path to the model, so no app can skip it |
| version history, quarantine, rewind | `hive/blob.rs`, `hive/mod.rs` | the kernel holds the values and knows which are tainted |
| `sent=` on context reads | `apps.rs`, `agent/mod.rs`, `prov_core::node_inputs` | records exactly what the model was shown, not the whole node |
| tasks, reminders, notes, ask, why, activity | `apps.rs`, `apps_cmd.rs`, `apps_core.rs` | everyday apps on the mediated path; reminders are the rules tier |
| threads: routing, focus, private threads | `threads.rs`, `threads_cmd.rs`, `threads_core.rs` | routing is a kernel rule; privacy is applied in `model_context` for every caller |
| protected `threads` node (peer writes refused) | `hive/mod.rs` | a peer must not be able to change what is private (a live run showed ignoring its value was not enough) |
| desktop double buffering | `vga_buffer.rs` | frames were captured mid-repaint |
| cluster coordinator and worker | `cluster.rs`, `cluster_core.rs`, `cluster_cmd.rs` | placement, checking and quarantine act on results that arrive as peer writes; decisions are provenance events |
| wake on mesh data (COM2 IRQ3) | `net/mod.rs` | lets idle nodes halt instead of polling, so CPU use reflects real work |

Pure logic (`prov_core`, `cap_core`, `sched_core`, `apps_core`, `threads_core`) uses only `core` + `alloc` and is
unit-tested on the host by the `hivemind-os-logic` crate, which compiles the kernel's
own source files.

## Not in the kernel (by design)
Lineage queries across instances, inference of cross-instance and read-from links,
the offline taint query over traces (the kernel keeps its own live taint bit), policy explanation from traces,
DAG scheduling with deadlines, configuration search, energy accounting.

## Backward compatibility
`write_blob`/`create_memory` keep their signatures (attributed to `kernel`); wire
formats HMSG and LLMREQ/LLMRSP are unchanged; capability enforcement is off unless
enabled; provenance lines on COM1 are ignored by older tools.
