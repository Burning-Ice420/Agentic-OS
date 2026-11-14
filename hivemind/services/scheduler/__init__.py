"""Agent workflow scheduler service (user space).

dag.py      task / workflow model, critical-path ranks
policies.py FIFO, priority, EDF, dependency-aware (least critical-path slack)
sim.py      discrete-event executor with resource slots, failures and retries
workload.py seeded synthetic workloads (durations can be derived from measured model latencies)
metrics.py  completion time, queue wait, deadline miss rate, throughput, utilization, fairness
real.py     executes DAGs with real local-model calls (measured, not simulated)
"""
