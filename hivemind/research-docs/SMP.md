# Multi-core: one HiveMind kernel on every CPU core

Before this, one HiveMind kernel used one CPU core, and the only way to use more cores
was the cluster (one VM per core). Now the kernel starts every core in the machine and
places jobs on them itself.
Full numbers: RESULTS.md, "Multi-core". Charts: `smp-report.html` (built by
`services/cluster/smp_report.py` from the benchmark record).

## Explain it in 30 seconds
"A computer has several CPU cores, but when it powers on only one is running. My kernel
finds the others in the firmware's tables, wakes each one up with a short piece of
start-up code, and gives them work. Core 0 stays in charge: it runs the shell and owns
the memory graph, so every security rule still has one place where it is enforced. The
other cores only crunch jobs. The kernel decides which core gets which job, and if one
core is slowed down, the free cores take its share. On 8 cores the same work finishes
5.7 times faster, and the host's own CPU meter confirms every core was busy."

## Results (`smp_bench-20261115-140657.json`, 9 vCPUs, medians of 3)
| What | Result |
|---|---|
| 48 prime jobs, 1 / 2 / 4 / 8 cores | 31.7 / 16.2 / 9.1 / 5.6 s: **5.7x** on 8 |
| hash, Collatz, tiny matmul, 1 vs 8 cores | 5.5x, 6.3x, 5.0x |
| one core 60% loaded: static vs dynamic | 11.1 vs 6.4 s: dynamic **1.75x** faster |
| correctness | 1,024 results, 0 wrong (896 checked against the host) |
| host check | guest busy time and host CPU time of QEMU agree within about 1% |
| pinned to 4 separate fast host cores | **3.9x on 4** (98% efficiency) |

## Questions you will get
**Why is it 5.7x and not 8x?** The laptop, not the kernel, and I tested it by pinning
QEMU to chosen host cores. On 4 separate fast cores the kernel gets 3.9x on 4 (98%
efficiency). But the laptop has only 8 physical cores: 4 fast ones and 4 efficiency
cores that are 1.38x slower per job. Its other 4 "CPUs" are hyperthreads, and two jobs
sharing one core through hyperthreads run at half speed each, so they add nothing for
this work. The best this machine can do with 8 compute cores is about 6.5x a fast core.
(`smp_pinning-20261115-161142.json`)

**Why not let every core run everything, like Linux?** That needs locks around the
memory graph, the devices and the agents, and per-core scheduling with preemption, which
is a much bigger change and easy to get subtly wrong. Keeping one owner core means the
provenance and taint rules did not change at all. Compute is the part that benefits from
more cores; the control work is small.

**What does the kernel actually decide?** Which core runs each job (static: fixed split
up front; dynamic: next free core takes the next job), when idle cores sleep and when
they are woken, and it records each run as a provenance event.

**Why does dynamic win when a core is loaded?** With a fixed split, core 1 still has
its 6 jobs while running at 40% speed, so the other 7 cores finish and sit idle (they
were 46 to 48% busy). With dynamic placement they keep taking jobs, and core 1 does fewer.

**How is this different from the cluster?** Same jobs, different trade-off. Here the
cores share memory, so handing over a job is nearly free (tiny jobs scale 5.0x; over the
cluster's serial link they scaled 1.6x). The cluster has separate kernels, so it checks
results, catches a lying worker and survives a VM dying. Shared memory means one trust
domain; separate VMs mean isolation.

**Is it real hardware?** Real x86 start-up protocol (ACPI MADT, INIT/SIPI, local APIC),
run under QEMU with Windows' hypervisor. Not tried on a bare machine.

## The design in one paragraph
Core 0 (the boot core) keeps everything it had: shell, devices, the hive memory graph,
agents, the model link. The other cores are **compute cores**. The kernel keeps a job
table in shared memory; a compute core takes a job, runs it, writes the result back and
halts when there is nothing left. Core 0 wakes the cores with an inter-processor
interrupt (IPI) when a run starts, and the last core to finish wakes core 0 the same way.
Idle cores are halted, not spinning, so an idle 9-core guest costs the host almost nothing.

This is an asymmetric design (one control core, N compute cores), not a general SMP
scheduler like Linux's. It is the smallest version that keeps the kernel's guarantees:
only core 0 touches the memory graph, so provenance and taint work exactly as before,
and the compute cores can only run the fixed, bounded job kinds.

## How the cores start (what happens at boot)
1. **Find the CPUs.** Read the ACPI tables the firmware leaves in memory: RSDP (found by
   its signature in 0xE0000..0xFFFFF) -> RSDT -> MADT, which lists every CPU's local
   APIC id. Parsing is pure code in `smp_core.rs`, tested on the host.
2. **Map the local APIC** (the per-core interrupt controller at 0xFEE00000), uncached.
3. **Pick a page below 1 MiB** for the start-up code. A starting core begins in 16-bit
   real mode and can only run code below 1 MiB. The kernel uses a page the bootloader
   used (no longer needed), never a page the memory allocator could hand out.
4. **Copy the trampoline** there and fill in the page table root, the entry point, and
   the core's stack. The trampoline switches the core 16-bit -> 32-bit -> 64-bit with the
   kernel's own page tables, then calls `ap_entry` in Rust.
5. **INIT, then SIPI** (start-up IPI) to each core in turn; wait until it reports online.
6. In `ap_entry` each core loads its own GDT and TSS (its own emergency stack for double
   faults), the shared interrupt table, enables its local APIC, and enters the job loop.

## Placement (who runs which job)
* `dynamic` (default): a free core takes the next waiting job. Load balances itself.
* `static`: job i runs on core i mod N, decided up front. Simple, but a slow core holds
  up the whole run.
`smp hog <core> <percent>` makes a core spend part of its time on a competing load so the
difference can be seen.

## Commands
```
cpus                         every core: state, jobs done, busy time
smp run prime 48             also hash, matmul, collatz, mixed
smp sched static|dynamic     placement
smp use 4                    use only the first 4 compute cores (0 = all)
smp hog 1 60                 core 1 spends 60% of its time on a competing load
smp bench prime 48           same jobs on 1, 2, 4 ... cores, prints a speedup table
smp results 10 | smp stop
```
Start a guest with several vCPUs: `.\run-os.ps1 -Cpus 4` (or QEMU `-smp 4`).

## Pieces
| Piece | File |
|---|---|
| ACPI parsing, static plan, busy/hog arithmetic (pure, host-tested) | `hivemind-os/src/smp_core.rs` |
| trampoline, core start-up, local APIC, job loop, runs | `hivemind-os/src/smp.rs` |
| shell commands | `hivemind-os/src/smp_cmd.rs` |
| wake-up IPI handler | `hivemind-os/src/interrupts.rs` (vector 0xF0) |
| benchmark | `experiments/smp_bench.py`, `experiments/smp_pinning.py` |
| report | `services/cluster/smp_report.py` |

## Cluster vs multi-core
They solve different problems and stack:
* **Multi-core** = one kernel, shared memory. Jobs move as pointers, no copying, no
  serial link, so even millisecond jobs scale. One trust domain: the cores share memory.
* **Cluster** = several kernels, no shared memory. Every result crosses a trust boundary,
  so the coordinator checks results and quarantines liars, and survives a VM dying.

## Limits (honest)
* Only core 0 runs the shell, agents, devices and the memory graph. Compute cores run
  only the bounded job kinds (prime, hash, matmul, collatz). Model calls stay on core 0.
* A job runs to completion on its core (no preemption); `smp stop` cancels between slices.
* One global spin lock protects the job table; fine for jobs of milliseconds or more,
  it would become a bottleneck for microsecond jobs.
* The competing load (`smp hog`) is simulated by the kernel, not a real second program.
* Page mappings are not changed after the cores start, so no TLB shootdown is needed;
  adding runtime unmapping would require one.
* Measured only under QEMU (WHPX) on one laptop, not on bare hardware.
