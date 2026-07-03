# HiveMind — Professor Fit & Outreach Mapping

Companion to `ayush-professor-outreach-plan.docx`. For each target: their real
recent work, the one-sentence overlap, and the **north star** — a concrete thing
to build in HiveMind that embeds their technique and can be *shown running*.
Grounded in web research (July 2026); cite the linked work in each email.

## The through-line to open every email with
**vLLM (OS paging → KV cache) → MemGPT (OS memory hierarchy → one agent's context)
→ HiveMind (the whole OS → a swarm of isolated agents).** MemGPT is co-authored by
*both* Gonzalez and Stoica, so this lineage lands with your two best targets and
signals you know the field. Your honest differentiation: MemGPT's "OS" is a
userspace metaphor for one agent; HiveMind is an *actual* OS for many, with
hardware isolation + a shared memory graph + audit, from scratch.

## Recommendation (who to contact, in order)
1. **Joseph Gonzalez (Berkeley Sky)** — best overall fit (MemGPT + LLMCompiler).
2. **Practical #1: his students** — MemGPT/Letta authors (Charles Packer, Sarah
   Wooders) and Gorilla's Shishir Patil. Same topic, far more reachable.
3. **Zhihao Jia (CMU)** — best pure-systems fit; lives on benchmarks/SLOs.
4. Then, artifact-permitting: Kraska → Percy Liang → Stoica (via students) →
   Madden → Lam → Balakrishnan → Riedl → Wei Xu.

The gate is identical for all of them: **a working demo + benchmark numbers.**

## The ONE composite demo (serves the top 3 at once)
"An isolated multi-agent orchestration runtime":
1. **Planner→executor DAG** (Gonzalez / LLMCompiler): planner splits a task into
   dependent agent-tasks; executors run in parallel across isolated OS instances;
   results merge through the shared memory graph.
2. **Tiered agent memory** (MemGPT / vLLM paging — Gonzalez + Stoica): hot blobs
   resident in RAM, cold blobs paged to disk/peer; demo page-in/page-out.
3. **SLO-aware placement scheduler** (Jia): under load, migrate agents across VMs
   to hold a latency/throughput SLO; measure attainment + isolation overhead.
4. **Observer + audit** (all): the DAG, paging, and rebalancing are visible and
   benchmarked.

## Per-professor mapping

| Professor | Real recent work | Overlap | North star to demo |
|---|---|---|---|
| **Joseph Gonzalez** (Berkeley) | LLMCompiler (planner→parallel-executor DAG), MemGPT, Gorilla, SkyRL | agent systems + OS-for-LLM | The **DAG + tiered-memory** pieces above |
| **Zhihao Jia** (CMU) | FlexLLM (SLO co-serving), SpecInfer, Mirage | systems scheduling/isolation/SLOs | **SLO agent-placement scheduler** + isolation-overhead benchmark |
| **Ion Stoica** (Berkeley) | vLLM/PagedAttention, Ray, MemGPT | OS abstractions for AI, distributed runtime | **OS paging for agent memory** (working-set benchmark); contact via students |
| **Tim Kraska** (MIT) | Palimpzest (declarative analytics), reproducible multi-agent (Tressoir) | agentic data systems, reproducibility | **Declarative agent workflows** over the hive with **reproducible re-execution + provenance** |
| **Percy Liang** (Stanford) | HELM (rigorous, reproducible eval) | evaluation/reproducibility | **HELM-style reproducible benchmark suite for agent runtimes** (= your benchmark harness, to his standard) |
| **Monica Lam** (Stanford OVAL) | GenieWorksheets (declarative, reliable agents), WikiChat | reliable/auditable assistants | **Reliable/auditable assistant**: every action grounded + provenance-tracked; declarative rules ↔ GenieWorksheets |
| **Samuel Madden** (MIT) | data systems (DSAIL) | memory graph as a database | **Memory graph as a benchmarked DB** — versioning, query, provenance overhead |
| **Hari Balakrishnan** (MIT) | Chord DHT, RON (resilient overlays) | distributed resilience | **Fault-tolerant shared memory** — kill a node, `Mirror` edges recover; measure recovery time (Chord→hive lineage) |
| **Mark Riedl** (Georgia Tech) | "Responsible AI Agents" (2025), explainable AI | explainability/safety | **Explainable + safe agents** — audit log + observer as an explainability tool; enforce safety constraints |
| **Wei Xu** (Georgia Tech) | LLM eval, long-context | agent-memory evaluation | **Long-horizon agent-memory evaluation** study (weakest fit) |

## Honest caveats
- **Fit gap:** none of the 10 are OS/kernel/isolation researchers — the list is
  AI-systems/data-systems/agents-heavy. Jia, Gonzalez, Stoica are closest, and even
  they work at the serving/orchestration layer, not the kernel. Consider adding
  2–3 systems-security / microVM / capability-OS faculty for a tighter "we built an
  OS" fit.
- **Numbers are the unlock**, not more emails. Build the composite demo + a
  reproducible benchmark, then contact the top 3–4.

## Sources
- Gonzalez / Sky Computing: https://sky.cs.berkeley.edu/publications/
- LLMCompiler: https://arxiv.org/pdf/2312.04511
- MemGPT / vLLM (PagedAttention): https://arxiv.org/abs/2309.06180
- Zhihao Jia: https://www.cs.cmu.edu/~zhihaoj2/
- Tim Kraska: https://people.csail.mit.edu/kraska/
- Stanford OVAL (Lam): https://oval.cs.stanford.edu/
- HELM (Liang): https://crfm.stanford.edu/helm/
- Riedl, "Responsible AI Agents": https://papers.ssrn.com/sol3/papers.cfm?abstract_id=5147666
- Balakrishnan (Chord/RON): https://people.csail.mit.edu/hari/
