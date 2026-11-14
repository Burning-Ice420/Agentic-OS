# Energy measurement method

Code: `services/energy/meter.py`, experiment: `experiments/phase5_energy.py`.

## Source
Windows exposes the CPU's RAPL counters through the Energy Meter Interface (EMI):

| Counter | Unit | Meaning |
|---|---|---|
| `\Energy Meter(rapl_package0_pkg)\Energy` | cumulative pWh (1 pWh = 3.6e-9 J) | whole SoC package: cores, uncore, iGPU |
| `\Energy Meter(rapl_package0_pp0)\Energy` | pWh | CPU cores |
| `\Energy Meter(rapl_package0_pp1)\Energy` | pWh | integrated GPU |
| `\Energy Meter(rapl_package0_dram)\Energy` | pWh | DRAM, reads 0 on this machine (not reported) |
| `...\Time` | ms | counter timestamp, used for window length |

`detect()` checks which rails actually advance over one second and records that in
every experiment record.

## What is measured, and what is not
* Measured: CPU-package energy over a window. Ollama runs on the CPU on this machine
  (iGPU rail stays near 0.07 W), so model inference energy shows up on the package and
  cores rails.
* Not measured: display, storage, memory, fan, power-supply losses, or anything else at
  the wall. Results are package joules, never "system" or "wall" energy.
* Not measured: carbon. `carbon_grams()` refuses to compute unless the caller passes a
  time stamped, location specific intensity and its source. No experiment in this
  repository passes one, so no carbon figure is reported.

## Procedure
1. One persistent PowerShell process answers counter reads, so a read does not start a
   process inside the measured window.
2. Every model used is loaded by an untimed call before its window (warm-up).
3. Idle baseline: three 20 s windows with nothing running from the harness, mean and
   standard deviation of package watts.
4. Each policy runs inside one window. Net energy = window joules minus idle watts times
   window seconds. Metric: net joules per successful task (a task counts only if the
   benchmark scorer accepts the answer).
5. Policy order is shuffled per repetition (seeded) so drift (thermal, background
   activity) is not attributed to one policy.

## Known sources of error
* This is a laptop with normal background processes; they are not stopped. Their
  variation lands in the idle baseline spread and the per-policy standard deviation.
* Idle subtraction assumes background power during the window equals the baseline
  mean. Short windows (the reflex policies) are most sensitive to this.
* Turbo and thermal state change across a run. Shuffling the order spreads this, it
  does not remove it.
* RAPL is a model-based estimate inside the CPU, not an external meter. It is the
  standard software-visible source, but it has its own error that is not quantified here.
