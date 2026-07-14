#!/usr/bin/env python3
"""
HiveMind AI-accelerator benchmark, MULTI system (swarm).

The single-system bench measures one agent's offloaded decision. This measures the
pattern HiveMind is actually about: agents on DIFFERENT nodes reacting to each
other's AI decisions through the shared memory graph. We test two shapes:

  1. Decision chain (pipeline): a sensor node's AI decision becomes the context for
     a coordinator node's AI decision, whose decision becomes the context for an
     actuator node. We measure whether the chain stays coherent end to end and the
     total latency across the hops.

  2. Consensus: several sensor nodes independently classify the same event; a
     coordinator takes the majority. We measure how often the swarm agrees.

Honesty: this isolates the AI decision quality across a coordination chain. The
mesh transport itself (COM2) is benchmarked elsewhere (the isolation test, and the
~55 ms reflex tick); here every node uses the same `call_ollama` the live bridge
uses, and one node's (key=value) decision is fed as the next node's context exactly
as a mesh HMSG blob would arrive.

Run (needs Ollama):  python ai-bench-multi.py
"""
import argparse, importlib.util, statistics, time, os

HERE = os.path.dirname(os.path.abspath(__file__))


def load_call():
    spec = importlib.util.spec_from_file_location("bridge", os.path.join(HERE, "hive-llm-bridge.py"))
    m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
    return m.call_ollama


CHAIN = [
    dict(node="A_sensor",      prompt="Is the system overheating? decide",
         state="temp=95,unit=celsius",
         accept=["high","alert","critical","yes","overheat","hot","danger"]),
    dict(node="B_coordinator", prompt="Given the upstream alert, should we shed load? yes or no",
         state="cluster_load=high",
         accept=["yes","shed","scale","throttle","reduce","true","offload"]),
    dict(node="C_actuator",    prompt="Choose one cooling action",
         state="fan=auto",
         accept=["throttle","cooldown","cool","reduce","spin","max","boost","increase","on"]),
]

CONSENSUS_STATE = "cpu=99,errors=rising,latency=spiking,queue=growing"
CONSENSUS_PROMPT = "Is this a production incident? yes or no"
CONSENSUS_ACCEPT = ["yes","incident","alert","true","critical","yep"]


def ok(accept, key, value):
    hay = f"{key} {value}".lower()
    return any(tok in hay for tok in accept)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", default="llama3.2:1b")
    ap.add_argument("--ollama", default="http://localhost:11434")
    ap.add_argument("--nodes", type=int, default=3, help="consensus sensor nodes")
    args = ap.parse_args()
    call = load_call()

    print(f"[ai-bench-multi] swarm | model={args.model}\n")
    call(args.ollama, args.model, "warmup", "say ready", "")  # warm the model

    # ---- Scenario 1: decision chain across nodes ----
    print("  Scenario 1: decision chain  A_sensor -> B_coordinator -> C_actuator")
    upstream, hops_ok, total = "", 0, 0.0
    for hop in CHAIN:
        ctx = (upstream + "," if upstream else "") + hop["state"]
        s = time.time()
        try:
            key, value = call(args.ollama, args.model, hop["node"], hop["prompt"], ctx)
        except Exception as e:
            key, value = "error", type(e).__name__
        dt = time.time() - s; total += dt
        good = ok(hop["accept"], key, value)
        hops_ok += 1 if good else 0
        print(f"    {'ok ' if good else 'miss'} {hop['node']:<14} state='{ctx}'  ->  {key}={value}   ({dt:.2f}s)")
        upstream = f"upstream_{key}={value}"  # this decision propagates to the next node
    coherent = hops_ok == len(CHAIN)
    print(f"    chain: {hops_ok}/{len(CHAIN)} hops coherent, total {total:.2f}s, "
          f"final action {'usable' if ok(CHAIN[-1]['accept'], *upstream.split('=',1)[::-1]) else 'unclear'}\n")

    # ---- Scenario 2: consensus across N sensor nodes ----
    print(f"  Scenario 2: consensus across {args.nodes} sensor nodes on one event")
    votes_yes, lat = 0, []
    for i in range(args.nodes):
        s = time.time()
        try:
            key, value = call(args.ollama, args.model, f"sensor_{i}", CONSENSUS_PROMPT, CONSENSUS_STATE)
        except Exception as e:
            key, value = "error", type(e).__name__
        lat.append(time.time() - s)
        yes = ok(CONSENSUS_ACCEPT, key, value)
        votes_yes += 1 if yes else 0
        print(f"    sensor_{i}: {key}={value}   -> {'INCIDENT' if yes else 'no'}   ({lat[-1]:.2f}s)")
    majority = "INCIDENT" if votes_yes * 2 > args.nodes else "no incident"
    agree = max(votes_yes, args.nodes - votes_yes)
    print(f"    consensus: {votes_yes}/{args.nodes} say incident -> swarm decides '{majority}', "
          f"agreement {agree}/{args.nodes}\n")

    print("  ================ MULTI-SYSTEM AI RESULT ================")
    print(f"  chain coherence  : {hops_ok}/{len(CHAIN)} hops, {'coherent end to end' if coherent else 'broke at a hop'}")
    print(f"  chain latency    : {total:.2f} s across {len(CHAIN)} nodes")
    print(f"  swarm consensus  : {votes_yes}/{args.nodes} agree it is an incident (majority = {majority})")
    print(f"  per-node latency : mean {statistics.mean(lat):.2f} s")
    print(f"  model            : {args.model}")
    print("  =======================================================")


if __name__ == "__main__":
    main()
