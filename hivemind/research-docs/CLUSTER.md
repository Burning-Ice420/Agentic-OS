# Hive cluster: several HiveMind VMs working at once

One coordinator VM and up to 8 worker VMs share work over the mesh. The
**coordinator's kernel decides placement**; workers run real compute; results land
in the coordinator's shared memory graph as untrusted peer writes until checked.
Results: RESULTS.md, "Hive cluster". Charts: `cluster-report.html` (built by
`services/cluster/report.py` from the benchmark record).

## Why it is here (how it serves the core idea)
The core idea is that the kernel mediates and records what crosses a trust boundary.
A cluster is the same problem at machine scale: work and results cross between VMs,
and a worker's result is untrusted input. So the coordinator kernel:
* places jobs using what it can see (jobs in flight, measured time per job, heartbeats),
* treats results as peer writes (taint applies) and re-runs a sample on another worker,
* quarantines a worker whose result does not match and re-runs everything it produced,
* re-routes the jobs of a worker that goes quiet,
* records every one of these decisions as provenance events.

## Pieces
| Piece | File | Role |
|---|---|---|
| job kinds, slicing, placement policies | `hivemind-os/src/cluster_core.rs` | pure, host-tested (7 tests) |
| coordinator and worker | `hivemind-os/src/cluster.rs` | job table, heartbeats, checks, quarantine, failover |
| shell commands | `hivemind-os/src/cluster_cmd.rs` | `cluster ...` |
| wake on mesh data | `net::enable_rx_wakeup` | COM2 receive interrupt (IRQ3), so idle nodes halt the CPU instead of polling |
| hub + live dashboard | `services/cluster/hub.py`, `dashboard.html` | forwards lines to the named VM, measures CPU per VM, serves http://127.0.0.1:8765 |
| benchmark | `experiments/cluster_bench.py` | 9 real guests, 16 runs, every result checked on the host |
| report | `services/cluster/report.py` | charts from a record |

Job kinds are a fixed set with bounded sizes (prime counting, hashing, integer matrix
product, Collatz, model calls, sum, AI report). No code travels over the mesh; an
out-of-range or unknown job is refused by the worker.

## Placement policies
* `rr`: job i to worker i mod N, decided without looking at load.
* `least`: the worker with the fewest jobs in flight (at most 2 each).
* `adaptive`: the worker expected to finish first, from its measured ms per job.

## Commands
```
cluster coord | cluster worker w1            role of this instance
cluster                                      workers, load, progress bar
cluster policy rr|least|adaptive
cluster use 4                                only the first 4 workers
cluster verify 25                            re-run 25% of results on another worker
cluster run prime 48 | hash | matmul | collatz | ai | mixed | pipeline 8
cluster results 10 | cluster reset | cluster evil on|off (demo)
```

## Run the demo
```
python experiments/cluster_bench.py --live      # boots 9 VMs, runs everything, keeps the dashboard
```
Open http://127.0.0.1:8765 while it runs. `--smoke` uses 2 workers and one run.

## Limits
* Each HiveMind kernel is single-core. The cluster spreads work over host cores by
  running one VM per core; the kernel does not schedule across cores (no SMP).
* The mesh is an emulated serial line. Jobs of a second scale well; jobs of a few
  milliseconds do not (1.6x to 2.1x on 8 workers).
* Sampled checking is probabilistic; 100% checking is certain but runs every job twice.
* The mesh has no authentication; a worker name is not a proven identity.
* One repetition per configuration on one laptop.
