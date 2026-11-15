# HiveMind OS

A small, from-scratch operating system (bare-metal Rust, x86_64) where the **kernel,
not the app, decides what reaches an AI model**, tracks which data is untrusted, and
records why every value exists.

* The model runs on the host (Ollama); the kernel owns the only path to it.
* Data from other machines is marked untrusted, and the mark follows it through agents
  and model answers. Policy can block it or leave it out of model calls.
* Private life areas (threads) never reach the model.
* Every write is attributed and its cause recorded: `why notes answer` in the OS.
* One kernel uses **every CPU core**: core 0 keeps the shell and memory graph, the kernel
  places jobs on the others (5.7x on 8 cores; `cpus`, `smp bench prime 48`).
* Several instances form a **cluster**: the coordinator's kernel places jobs on worker VMs,
  checks their results and survives a worker dying (7.1x on 8 workers; live dashboard).
* Everyday apps on top: tasks and reminders, notes you can ask, threads and focus, an
  activity view, a text-mode desktop.

**Start with [research-docs/OVERVIEW.md](research-docs/OVERVIEW.md)**: what it is, why
the kernel, how it compares with existing systems, and its limits.

| | |
|---|---|
| Results (all measured, including failures) | [research-docs/RESULTS.md](research-docs/RESULTS.md) |
| Architecture | [research-docs/ARCHITECTURE.md](research-docs/ARCHITECTURE.md) |
| Threat model | [research-docs/THREAT_MODEL.md](research-docs/THREAT_MODEL.md) |
| Cluster guide and charts | [research-docs/CLUSTER.md](research-docs/CLUSTER.md), [research-docs/cluster-report.html](research-docs/cluster-report.html) |
| Multi-core guide and charts | [research-docs/SMP.md](research-docs/SMP.md), [research-docs/smp-report.html](research-docs/smp-report.html) |
| Using the apps | [research-docs/APPS_AND_TAINT.md](research-docs/APPS_AND_TAINT.md), [research-docs/THREADS.md](research-docs/THREADS.md) |
| Rebuild and rerun everything | [research-docs/REPRODUCIBILITY.md](research-docs/REPRODUCIBILITY.md) |

## Run it (Windows, QEMU with WHPX)
```
cd hivemind-os
cargo bootimage
.\run-os.ps1 -LLM                                  # OS with model device and mesh
python hive-llm-bridge.py --model llama3.2:3b      # second terminal, needs Ollama
```
In the OS: `help`, `help apps`, `help thread`, `help smp`, `ui`. Add `-Cpus 4` for several cores.

## Layout
* `hivemind-os/` the kernel (`src/`), boot scripts, model bridge, peer simulator
* `hivemind-os-logic/` host unit tests that compile the kernel's own pure-logic files
* `services/` host tools: provenance lineage, scheduler, config search, energy
* `experiments/` harnesses that drive real guests; JSON records in `experiments/results/`
* `research-docs/` the documents above
