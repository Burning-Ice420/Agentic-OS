# PROJECT_CONTEXT.md

Ground truth for anyone (human or agent) working on this repository: what exists, what
is measured, what is open. Built from the code, docs and records on **2026-11-16**
(commit `13ed409`). The code is the source of truth; if this file and the code disagree,
the code wins and this file must be updated. Update it with dates and evidence when
anything significant changes.

---

## 1. Name and repository identity

| | |
|---|---|
| Project name used in code and docs | **HiveMind** (HiveMind OS) |
| GitHub repository | `Burning-Ice420/Agentic-OS`, default branch `main` |
| Local folder | `RustOS` (the git root; the project itself is in `hivemind/`) |
| "RESTOS" | Same project as HiveMind (owner confirmed 2026-11-16). The name appears nowhere in the repository |
| License | **None.** No LICENSE file exists |
| CI | **None.** No `.github/` workflows; all checks below are run by hand |
| Commits | 59, first on 2026-06-26 |

## 2. What it is (one paragraph, from the code)

HiveMind is a small operating system written from scratch in `no_std` Rust for x86-64
(about 10,200 non-comment lines in `hivemind-os/src`), booted under QEMU with Windows'
hypervisor (WHPX). Its central kernel object is a **memory graph**: named nodes holding
values, linked by typed edges. Apps, in-kernel agents (rule sets, not arbitrary code),
the language model and other machines all read and write through that graph, and every
write is attributed to an actor. On top of that the kernel **mediates every model call**:
it builds the context the model sees, applies a taint policy (untrusted data from other
machines can be blocked or left out), never sends items from private threads, checks
the model's answer with the calling agent's capabilities, and records the cause of every
value as provenance. The model itself runs on the host (Ollama) behind a serial device.
Several instances can share work as a cluster, and one instance can use every CPU core.

## 3. Architecture (current)

```
 one HiveMind VM (QEMU + WHPX, 1..N vCPUs)                           host (Windows)
 ┌──────────────────────────────────────────────────────────┐
 │ core 0                                                   │
 │  shell, desktop (text mode), apps: tasks, notes, threads │
 │  agents (edge-triggered rules) ──┐                       │
 │                                   ▼                      │
 │  memory graph (hive) ── every write: actor, taint bit    │
 │     │                                                    │
 │     ├─ capabilities (read / write / model, expiry)       │
 │     ├─ provenance ring ──────────────── COM1 ───────────────► lineage tools, harnesses
 │     └─ model_context(): taint policy, private threads    │
 │            └─ offload ───────────────── COM3 ───────────────► bridge ─► Ollama (model)
 │  mesh (graph sync, cluster jobs) ────── COM2 ───────────────► hub ─► other VMs
 │  ATA disk: graph + taint persisted                       │
 │ cores 1..N: bounded compute jobs only (SMP)              │
 └──────────────────────────────────────────────────────────┘
```

Repository layout (`hivemind/`):

| Path | What it is | Status |
|---|---|---|
| `hivemind-os/` | the bare-metal kernel, boot scripts, model bridge, peer simulator | **current; everything evaluated lives here** |
| `hivemind-os-logic/` | host crate that compiles the kernel's pure-logic files and unit-tests them | current, 53 tests |
| `experiments/` | harnesses that boot real guests and drive them by keystrokes; JSON records in `experiments/results/` | current |
| `services/` | host tools: provenance lineage, scheduler sim, config search, energy, cluster hub and dashboard | current, 38 Python tests |
| `research-docs/` | OVERVIEW, ARCHITECTURE, RESULTS, THREAT_MODEL, guides | current (updated 2026-11-15) |
| `hivemind-kernel/`, `hivemind-vos/` | **early host-side prototype** (tokio/axum "hive" with HTTP API on :8080, VM launcher). Unchanged since the first commit (2026-06-26); not part of any current evaluation | stale |
| `hivemind-observer/` | egui desktop viewer polling :8080, fed by `hive-observer-bridge.py` | **stale** (July 2026); sees only mesh broadcasts and model calls (section 6) |
| `../docs/` | owner's local notes, paper draft, outreach drafts | **gitignored**, local only |

## 4. Features: implemented, experimental, planned, absent

**Implemented and measured on real guests** (records in `experiments/results/`, numbers in `research-docs/RESULTS.md`):

| Feature | Evidence |
|---|---|
| Memory graph, actor-attributed writes, provenance ring, `why` | phase 1+2, 14/14 checks; 0 dropped events |
| Capabilities: per-agent read, write, model call; expiry, revocation, delegation | 14/14; check 345 ns to 2.3 µs |
| Taint: per value, through agents and model answers, saved to disk | apps run, 19/19 |
| Model-call policy `off` / `block` / `redact`; `quarantine`, `rewind` | same run: injected note obeyed under `off`, refused under `block`, left out under `redact` |
| Private threads, routing rules, focus | 18/18 (first run 11/17 leaked private data; fixed and rerun) |
| Cross-machine lineage over the mesh | 9/9 |
| Hive cluster: placement, sampled checking, quarantine, failover; multi-core workers | 7.1x on 8 VMs; liar caught; VM kill survived; 0 wrong |
| Multi-core (SMP): ACPI MADT, INIT/SIPI, compute cores | 3.9x on 4 independent cores, 5.7x on 8 vCPUs; 0 wrong |
| Everyday apps, text-mode desktop, ATA persistence, boots in 16 MiB | apps run |

**Experimental or partial:**
- Workflow scheduler policies: simulated plus one small real run, **inconclusive**.
- Config search (Bayesian vs random): no difference at the budget tried.
- Learned latency predictor: **negative** (no better than a per-model mean).
- Energy: rules-first about 2.4x less energy per successful task; partly by construction.
- Cluster-with-multi-core-workers timings: **possibly throttled** (laptop found on battery).

**Absent (not built), stated in the docs:**
- Per-agent hardware isolation. Agents inside one instance share one kernel; isolation
  exists only between VMs. Per-agent VM density was never measured.
- Any head-to-head comparison with other agent systems, containers (Docker, gVisor) or
  microVMs (Firecracker). The positioning doc's planned Firecracker benchmark was never run.
- A baseline comparing the memory graph with process-, file-, message- or actor-based
  coordination (see section 5, H0).
- Mesh authentication; conflict detection for concurrent writes; a shared-memory or
  virtio event channel; preemptive scheduling of agents across cores.
- License, CI.

## 5. Core hypotheses and design invariants

**Hypotheses.** The owner chose to pursue **both H0 and H1** (2026-11-16). H1 is the one
current docs and the paper draft argue for; H0 is **not yet tested by anything in the
repository**.

| | Hypothesis | Status |
|---|---|---|
| H0 | A kernel-resident shared memory graph is a better abstraction for coordinating isolated agents than the process-and-file model | **untested**: no baseline, no comparative workload |
| H1 | Mediating the model call in the kernel gives one place to see, restrict and explain what reaches a model, which app-level checks cannot guarantee | supported by live runs (section 4); no comparison with runtime-level defences (CaMeL, FIDES) |
| H2 | Rules first, model second, is cheaper and gives the model fewer chances to be misled | energy result; partly by construction |

**Invariants the code currently maintains** (changing any needs owner approval, section 9):
1. Every graph write goes through `write_blob_as` / `write_blob_tx` and records an actor.
2. Every model call builds its context in `Hive::model_context`; policy and private-thread
   filtering are applied there, for apps and agents alike.
3. A model answer is written as "model acting for agent X" and checked with X's rights.
4. Values from the mesh are written as `peer` and carry the taint bit; the bit propagates.
5. Peer writes to the `threads` node are refused.
6. Only core 0 touches the graph; other cores run only the bounded job kinds in
   `cluster_core::Kind`. No code crosses the mesh.

## 6. Known limitations and open questions

- **Conflicting writes: last arrival wins.** Verified in `net/mod.rs::poll_and_apply`: an
  incoming mesh write overwrites the local value; there are no versions, clocks or
  conflict detection (the `threads` node refusal is the only exception). Effects of
  concurrent writers have not been measured.
- **Whole-node offload gap:** an agent's model call sends the whole node, not only the
  keys its rule depends on, so untrusted text can reach the model under policy `off`.
  Detected, not blocked (THREAT_MODEL.md, "Known gap").
- Taint is per value, not per character, and conservative; typed input is trusted.
- The mesh has no authentication: a peer's name is not a proven identity.
- Provenance shipping over emulated serial costs about 1.3 ms per event.
- Rollback history does not survive a reboot (quarantine can still remove).
- Single laptop, one model family, mostly single live runs.
- Benchmark power state was not recorded before 2026-11-15 evening; one run was on battery.
- **The observer is stale:** it reads COM2 and COM3 only, not the COM1 provenance stream,
  so it does not show attribution, taint, policy decisions, capability denials, threads,
  cluster or cores, and misses local agent writes entirely.
- Open: is the memory graph (H0) actually better than simpler coordination models, and
  on which workloads? How does kernel-level mediation compare with CaMeL-style runtime
  defences on the same attacks?

## 7. Commands verified on 2026-11-16 (this machine)

| What | Command (from `hivemind/`) | Result |
|---|---|---|
| Kernel pure-logic tests | `cargo test -p hivemind-os-logic` | 53 passed |
| Host prototype tests | `cargo test -p hivemind-vos` / `-p hivemind-kernel` | 18 / 0 tests, pass |
| Python service tests | `python -m pytest -q services/tests` | 38 passed |
| Build the observer | `cargo build -p hivemind-observer` | builds |
| Build the kernel | `cd hivemind-os && cargo bootimage` (needs `~/.cargo/bin` on PATH, nightly) | builds |
| Run one VM with the model | `.\run-os.ps1 -LLM` plus `python hive-llm-bridge.py --model llama3.2:3b` | used throughout; not rerun today |
| Benchmarks | `python experiments/<name>.py`, list in `research-docs/REPRODUCIBILITY.md` | each run at least once on 2026-11-14/15; not rerun today |

Benchmarks need the laptop plugged in and otherwise idle; records now store the power state.

## 8. Research topics that map to real design questions

| Design question in this repo | Research area | Why it matters here |
|---|---|---|
| Last-arrival-wins mesh writes (section 6) | replicated data types (CRDTs), causal consistency, version vectors | concurrent agents on different VMs can silently overwrite each other today |
| H0: graph vs process/file/message/actor coordination | tuple spaces and blackboard systems, actor runtimes, shared-memory vs message passing | needed to test H0 at all; the graph may lose on some workloads |
| H1: kernel mediation of model calls | information flow control (HiStar, Asbestos, Flume), CaMeL, FIDES, prompt-injection evaluations | the closest prior work; a fair comparison is missing |
| Whole-node offload gap | fine-grained IFC, least-privilege context construction | the one open safety gap |
| Mesh has no identity | authenticated channels, capability systems across machines | taint currently assumes "from the mesh" = untrusted, without knowing who |
| Provenance cost over serial | provenance capture systems (OS-level provenance), shared-memory tracing | 229x cost when shipped synchronously |
| Asymmetric multi-core design | multikernels, asymmetric/partitioned OS designs | justify or replace the core-0-owns-the-graph choice |
| Evaluation method | agent-security benchmarks, systems benchmarking methodology | most results are single runs on one laptop |

## 9. Areas that need explicit owner approval before modification

- Kernel semantics of the memory graph, write path, attribution (invariants 1, 4, 5).
- `model_context`, taint propagation, policy, private threads (invariants 2, 3).
- Capabilities and any security boundary; mesh protocol; cluster checking and quarantine.
- SMP: core start-up, what non-zero cores may run (invariant 6); scheduling.
- Persistence format on disk.
- Repository settings, history rewriting, licensing.
- **Push policy (owner decision 2026-11-16):** keep pushing to `main` after checks pass.
  This overrides the research brief's "never push to main" rule. Commits carry no
  co-author line. Changes in the protected areas above still need the owner's approval
  before they are made.

## 10. Uncertainties to confirm with the owner

Resolved 2026-11-16: RESTOS = HiveMind; pursue both H0 and H1; keep pushing to `main`.

1. **The stale host prototype** (`hivemind-kernel`, `hivemind-vos`) and the observer: keep,
   rebuild, or mark as archived?
2. **License:** none is set; one is needed before others can legally reuse the code.
