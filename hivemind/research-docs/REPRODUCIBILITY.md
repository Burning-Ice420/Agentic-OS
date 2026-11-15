# Reproducing every result

All experiment scripts write a JSON record to `hivemind/experiments/results/` with the
git commit, dirty flag, OS, CPU, RAM, tool versions, configuration, seeds, repetitions,
warm-up policy, failed runs and labelled metrics (measured / derived / simulated /
estimated / proposed).

## Prerequisites
* Windows 11 with Windows Hypervisor Platform (WHPX); QEMU at `C:\msys64\mingw64\bin`.
* Rust nightly (pinned by `hivemind-os/rust-toolchain.toml`), `cargo install bootimage`.
* Python 3.14 with numpy, scipy. Ollama with `llama3.2:1b` and `llama3.2:3b`.
* For energy: a CPU whose RAPL counters are exposed through Windows EMI
  (`Get-Counter -ListSet 'Energy Meter'`).

## Build
```
cd hivemind/hivemind-os
cargo bootimage
copy target\x86_64-hivemind-os\debug\bootimage-hivemind-os.bin C:\hivemind\boot_p.bin
```

## Tests
```
cd hivemind
cargo test -p hivemind-os-logic          # kernel pure logic: provenance, capabilities, scheduling
cargo test -p hivemind-kernel -p hivemind-vos
python -m unittest discover -s services/tests -t .
```

## Experiments
| Result | Command | Label |
|---|---|---|
| Live provenance + capabilities on a real guest | `python experiments/phase12_live.py` | measured |
| Kernel overhead (provenance, capability checks) | `python experiments/phase12_overhead.py` | measured |
| Scheduler policies, 3 loads × 10 seeds | `python experiments/phase3_sched.py sim` | simulated |
| Scheduler with real model calls | `python experiments/phase3_sched.py real` | measured |
| Scheduler config search + transfer | `python experiments/phase4_opt.py sched` | simulated |
| AI-bridge config search | `python experiments/phase4_opt.py ai` | measured |
| Energy per successful task | `python experiments/phase5_energy.py --reps 3` | measured |
| Latency predictor vs baselines | `python experiments/phase6_ml.py` | derived |
| Untrusted content reaching a model tool (MVP) | `python experiments/mvp_injection.py` | measured |
| Lineage across two guests on one mesh | `python experiments/two_vm_mesh.py` | measured |
| Everyday apps, taint block/redact/quarantine, two boots | `python experiments/apps_live.py` | measured |
| Life threads: routing, focus, private threads, two boots | `python experiments/threads_live.py` | measured |
| Hive cluster: 9 VMs, scaling, placement, checking, failover | `python experiments/cluster_bench.py` (`--live` keeps the dashboard) | measured |
| Multi-core: one guest with 9 vCPUs, scaling, kinds, imbalance | `python experiments/smp_bench.py`, then `python services/cluster/smp_report.py <record>` | measured |
| Cluster with multi-core workers: 8 cores split 8x1, 4x2, 2x4, 1x8 | `python experiments/cluster_cores_bench.py` (plug the laptop in) | measured |
| Multi-core with QEMU pinned to performance, efficiency or hyperthread cores | `python experiments/smp_pinning.py` (logical CPU map is for the i5-13420H; edit it for another host) | measured |

Phases 3 real to 6 and the MVP need Ollama with `llama3.2:1b` and `llama3.2:3b`
pulled (`ollama list`). Run guest experiments one at a time: each starts by killing
every QEMU process.

## Sources of variance you should expect
* Guest timing runs under WHPX virtualisation.
* Model latency depends on what else the laptop is doing; energy includes background
  load (reported as idle baseline ± sd and per-run sd).
* Local-model answers vary at temperature > 0; accuracy granularity is 1/13.
* Keystroke injection needs ~0.28 s per key; monitor paths must use forward slashes.
