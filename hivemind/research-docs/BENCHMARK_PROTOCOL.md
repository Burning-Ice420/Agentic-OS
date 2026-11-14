# Benchmark protocol

Rules every number in RESULTS.md follows.

## Labels
Every metric in a record carries exactly one label:

| Label | Meaning |
|---|---|
| measured | observed on real hardware or a real guest in this repository |
| derived | computed from measured values (a mean, a ratio, a regret) |
| simulated | produced by the discrete event simulator, never by a real run |
| estimated | an input assumed, not observed (none are used for headline results) |
| proposed | a design that is not implemented or not evaluated |

A simulated result is never quoted as if it were measured. Simulator inputs that come
from measurement (model call latency) are stated as such.

## Records
`experiments/record.py` writes one JSON per run with: git commit and dirty flag, OS,
CPU, RAM, Python, Rust, QEMU and Ollama versions, the full configuration, seed,
repetitions, warm-up policy, failed runs with reasons, and the labelled metrics. A run
that fails is kept in `failed_runs`, never silently retried away.

## Per phase

**Provenance and capabilities (live).** One real guest boot, scripted shell input via
the QEMU monitor, PEV lines read from the COM1 log. Pass or fail per check, plus
drops, id gaps and malformed lines. The log is saved as a test fixture.

**Overhead (live, in guest).** `bench prov N` and `bench cap N C` run inside the kernel
and time with the TSC, calibrated against the PIT in the same boot. Each configuration
is repeated (default 3) and reported as mean and standard deviation. Timing is inside a
WHPX virtual machine, so absolute numbers include virtualization cost; ratios between
configurations in the same boot are the meaningful result.

**Scheduler.** Simulated: 4 to 5 policies, 3 load levels, 10 seeds, mean and 95% CI.
Real: a small workload of real local model calls per policy, repeated, wall clock.

**Configuration search.** Simulated scheduler config: an exhaustive grid gives the
true optimum, so random and Bayesian search are scored by regret at equal budget across
10 seeds. Transfer is scored by evaluating workload A's optimum on workload B. Real AI
bridge config: every proposal is a real model run, accuracy under a latency ceiling.
Invalid configurations (forbidden security keys, resource ceiling) are rejected by the
validator before evaluation and never run.

**Energy.** See ENERGY_METHOD.md. Idle subtracted package joules per successful task,
policy order shuffled per repetition.

**Learned prediction.** 5-fold cross validation, seeded, against the simple baselines
(global mean, per model mean). Only features known before the call are allowed.

## Things not done
* No comparison against another operating system or agent framework. Baselines are
  simple policies inside this system.
* No statistical claim is made from fewer than 3 repetitions; where repetitions are low
  the record says so.
* Hardware is one laptop. Nothing is claimed to generalize beyond it.
