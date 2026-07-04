#!/usr/bin/env python3
"""
HiveMind AI-accelerator bridge.

The bare-metal OS cannot run a neural network, so it offloads reasoning to a
small *local* model the way a kernel offloads compute to a GPU/NPU. This bridge
is that accelerator: it speaks the OS's COM3 line protocol and runs a tiny model
via Ollama.

  OS  -> bridge:  LLMREQ|<memory>|<prompt>|<key=val,key=val,...>
  bridge -> OS:   LLMRSP|<memory>|<key>|<value>

QEMU exposes the guest's COM3 as a TCP server (run-os.ps1 -LLM). This bridge
connects to it as a client.

Setup (one time):
    winget install Ollama.Ollama      # or https://ollama.com/download
    ollama pull llama3.2:1b           # ~1.3 GB, CPU-friendly  (or qwen2.5:0.5b)

Run:
    python hive-llm-bridge.py                     # defaults: llama3.2:1b, :4455
    python hive-llm-bridge.py --model qwen2.5:0.5b
"""

import argparse
import json
import re
import socket
import sys
import time
import urllib.request

SYSTEM_PROMPT = (
    "You are the reasoning coprocessor for a kernel agent. You are given a memory "
    "node's current state and a task. Decide ONE action and reply with EXACTLY one "
    "line of the form key=value (a short snake_case key and a short value). "
    "No explanation, no punctuation, just key=value."
)


def call_ollama(host, model, memory, prompt, context):
    """Return (key, value) from the local model, or None on failure."""
    user = f"Memory node '{memory}' state: {context or '(empty)'}. Task: {prompt}."
    body = {
        "model": model,
        "prompt": f"{SYSTEM_PROMPT}\n\n{user}\n\nAction:",
        "stream": False,
        "options": {"temperature": 0.2, "num_predict": 32},
    }
    req = urllib.request.Request(
        f"{host}/api/generate",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=60) as r:
        out = json.loads(r.read().decode()).get("response", "")
    # Lenient parse: first key=value in the model's output.
    m = re.search(r"([A-Za-z][\w\-]{0,30})\s*=\s*([^\n,;|]{1,40})", out)
    if m:
        return m.group(1).strip(), m.group(2).strip()
    # Fallback: stash a short note.
    note = re.sub(r"[|\n\r]", " ", out).strip()[:40] or "no_output"
    return "ai_note", note


def rule_fallback(memory, prompt, context):
    """Used when Ollama is unreachable, so the demo still shows a response."""
    ctx = context.lower()
    if "temp=" in ctx:
        try:
            t = int(re.search(r"temp=(\d+)", ctx).group(1))
            return ("alert", "HIGH" if t > 80 else "ok")
        except Exception:
            pass
    return ("ai_verdict", "healthy")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", default="llama3.2:1b")
    ap.add_argument("--ollama", default="http://localhost:11434")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=4455)
    args = ap.parse_args()

    print(f"[bridge] connecting to guest COM3 at {args.host}:{args.port} ...")
    sock = None
    for _ in range(120):
        try:
            sock = socket.create_connection((args.host, args.port))
            break
        except OSError:
            time.sleep(1)
    if sock is None:
        sys.exit("[bridge] could not connect — is the VM running with -LLM?")
    print(f"[bridge] connected. model={args.model} (Ollama at {args.ollama})")

    buf = b""
    while True:
        data = sock.recv(4096)
        if not data:
            print("[bridge] guest disconnected.")
            return
        buf += data
        while b"\n" in buf:
            line, buf = buf.split(b"\n", 1)
            text = line.decode("utf-8", "ignore").strip()
            if not text.startswith("LLMREQ|"):
                continue
            parts = text[len("LLMREQ|"):].split("|", 2)
            memory = parts[0] if len(parts) > 0 else "kernel-root"
            prompt = parts[1] if len(parts) > 1 else "decide"
            context = parts[2] if len(parts) > 2 else ""
            print(f"[bridge] REQ  {memory}: task='{prompt}' state='{context}'")
            try:
                key, value = call_ollama(args.ollama, args.model, memory, prompt, context)
                src = args.model
            except Exception as e:
                key, value = rule_fallback(memory, prompt, context)
                src = f"fallback ({type(e).__name__})"
            resp = f"LLMRSP|{memory}|{key}|{value}\n"
            sock.sendall(resp.encode())
            print(f"[bridge] RSP  {memory}: {key}={value}   [{src}]")


if __name__ == "__main__":
    main()
