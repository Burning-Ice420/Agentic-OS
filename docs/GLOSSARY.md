# HiveMind — Glossary (every keyword, in plain English)

Written for someone who is *not* a systems person. Every term used anywhere in
this project is defined here in one or two sentences, followed by *"In HiveMind:"*
— how it actually shows up in our code. Skim the section headers; you don't need
to read top to bottom.

Companion to `hivemind-os-knowledge-dump.md` (the from-zero narrative) and
`POSITIONING.md` (the research thesis).

---

## 1. The big picture

- **Operating System (OS)** — the software that controls a computer's hardware
  (CPU, memory, disk, keyboard) and runs programs. Windows and Linux are OSes.
  *In HiveMind:* we wrote our own tiny OS from scratch in Rust.
- **Kernel** — the innermost core of an OS; it has full control of the hardware.
  Everything else runs "on top of" the kernel. *In HiveMind:* our whole OS is
  basically one kernel — there's no separate userspace yet.
- **Bare-metal** — software that runs directly on the hardware with no OS
  underneath it. *In HiveMind:* our OS is bare-metal — it *is* the thing that
  boots, nothing is under it.
- **From scratch** — built ourselves, not by reusing Linux/Windows. This is the
  whole point of the project (and, for security, a feature: a tiny codebase you
  can actually audit).
- **Guest / Host** — in virtualization, the **host** is your real machine; a
  **guest** is a virtual computer running inside it. *In HiveMind:* your Windows
  PC is the host; each HiveMind OS instance is a guest inside QEMU.

## 2. Rust & building

- **Rust** — a modern programming language known for memory safety without a
  garbage collector; popular for OS and systems work.
- **`no_std`** — Rust code that does *not* use the standard library (because there's
  no OS to provide it). *In HiveMind:* the OS is `no_std` — it only has `core` and
  a hand-rolled heap.
- **Crate** — a Rust package/library. *In HiveMind:* the workspace has four crates
  (`hivemind-os`, `-kernel`, `-vos`, `-observer`).
- **Cargo** — Rust's build tool.
- **`cargo bootimage`** — a tool that turns our kernel into a bootable disk image
  QEMU can start.
- **Boot image** — a single file that a (virtual) computer can boot from, like a
  bootable USB. *In HiveMind:* `bootimage-hivemind-os.bin`.
- **Bootloader** — the small program that runs first, sets up the CPU, and hands
  control to your kernel's `kernel_main()`.
- **Target / target triple / `.json` target** — a description of the CPU + ABI you
  compile for. *In HiveMind:* `x86_64-hivemind-os.json` — a custom target with SSE
  disabled and soft-float (see below).
- **Toolchain (stable/nightly)** — a specific version of the Rust compiler. Our OS
  needs **nightly** for some unstable features; the host crates use **stable**.

## 3. CPU, boot, and interrupts

- **x86-64** — the 64-bit CPU architecture used by most PCs.
- **Interrupt** — a hardware "tap on the shoulder": the CPU stops what it's doing,
  runs a small handler, then resumes. Used for the keyboard, timer, mouse, etc.
- **IRQ (Interrupt Request)** — a numbered hardware interrupt line (IRQ0 = timer,
  IRQ1 = keyboard, IRQ12 = mouse, IRQ14 = disk).
- **Polling** — the opposite of interrupts: repeatedly *asking* "any input yet?" in
  a loop. Simple but wasteful, and it misses input while you're busy. *In HiveMind:*
  the original keyboard bug was that it polled; we made it interrupt-driven.
- **IDT (Interrupt Descriptor Table)** — a table mapping each interrupt number to
  the function that handles it.
- **PIC (Programmable Interrupt Controller)** — the chip that routes the 16 hardware
  IRQ lines to the CPU. You "mask" (disable) or "unmask" (enable) each line.
- **PIT (Programmable Interval Timer)** — the chip that fires the timer interrupt at
  a fixed rate (~18 times/sec by default). *In HiveMind:* drives the system `tick`.
- **EOI (End Of Interrupt)** — the "I'm done" signal a handler sends back to the PIC
  so it will deliver the next interrupt. Forget it and interrupts stop.
- **`sti` / enabling interrupts** — the CPU instruction that turns interrupts on.
  *In HiveMind:* the original code never ran it — the root cause of the keyboard
  "breaking."
- **GDT (Global Descriptor Table)** / **TSS (Task State Segment)** / **IST (Interrupt
  Stack Table)** — low-level CPU tables. The IST gives certain fault handlers a
  known-good separate stack so they still work even if the normal stack is broken.
- **Exception** — a fault the CPU raises when something goes wrong, each with a name:
  - **#UD (Invalid Opcode)** — CPU tried to run an instruction it can't. *In HiveMind:*
    `movaps` faulted here because SSE was off.
  - **#GP (General Protection Fault)** — a protection rule was violated.
  - **#PF (Page Fault)** — accessed memory that isn't mapped.
  - **#NP (Segment Not Present)** — referenced a table entry that isn't there. *In
    HiveMind:* the ATA IRQ14 hitting an empty IDT slot.
  - **Double fault** — a fault happened *while handling another fault*.
  - **Triple fault** — a fault while handling a double fault → the CPU gives up and
    resets/halts.
- **Spurious interrupt** — a stray/unexpected interrupt with no real source; must be
  handled gracefully or it can crash you.
- **SSE / XMM registers** — a set of CPU instructions/registers for math and bulk
  memory moves (`movaps` is one). Must be enabled in the CPU to use.
- **Soft-float** — doing floating-point math in software instead of using SSE
  hardware. *In HiveMind:* we build the OS soft-float so the compiler never emits
  SSE (`movaps`) in interrupt handlers, which was crashing them.
- **ABI (Application Binary Interface)** — the rules for how compiled code passes
  arguments, uses registers, etc. `rustc-abi: x86-softfloat` tells the compiler our
  ABI is soft-float.
- **CR0 / CR4** — CPU control registers whose bits enable features like SSE.

## 4. Memory

- **RAM** — the computer's fast working memory, erased on power off.
- **Heap** — the region of memory used for dynamically-sized data (Vecs, Strings).
  *In HiveMind:* a 2 MiB kernel heap.
- **Allocator** — the code that hands out and reclaims heap memory.
- **Paging / page table** — the CPU mechanism that maps virtual addresses (what code
  sees) to physical addresses (real RAM), in 4 KiB "pages."
- **Frame allocator** — hands out physical 4 KiB "frames" of RAM for the page tables
  and heap. *In HiveMind:* we replaced an O(n²) one with an O(1) bump allocator.
- **Bump allocator** — the simplest allocator: keep a pointer and move it forward for
  each allocation. Fast (O(1)).
- **`.bss`** — the zero-initialized memory region of a program (doesn't take up space
  in the file). *In HiveMind:* the 32 KB scrollback buffer lives here so it doesn't
  overflow the tiny boot stack.
- **Atomic** — a value that can be updated in one indivisible CPU instruction, so it's
  safe to touch from an interrupt without a lock. *In HiveMind:* the `TICKS` counter.
- **Mutex / lock / spinlock** — a way to let only one piece of code use shared data at
  a time; a *spinlock* "spins" in a loop until it's free.
- **Deadlock** — two pieces of code each waiting for a lock the other holds → frozen
  forever. *In HiveMind:* `TICKS` as a Mutex could deadlock the timer interrupt; we
  made it atomic.

## 5. Devices & drivers

- **Driver** — code that talks to a specific piece of hardware.
- **Port I/O** — reading/writing hardware by talking to numbered "I/O ports."
- **VGA text mode** — an old, simple display mode: an 80×25 grid of characters stored
  in memory at address `0xb8000`. *In HiveMind:* our whole UI is drawn here.
- **CP437** — the old IBM PC character set that includes box-drawing and shade glyphs
  (`░ █ ╔ ═`). *In HiveMind:* used for the desktop windows and mouse cursor.
- **Scrollback** — remembering lines that scrolled off screen so you can scroll up.
  *In HiveMind:* a 200-line ring buffer; PageUp/PageDown.
- **Serial port / UART / COM1-COM4** — a simple one-byte-at-a-time communication
  channel (like an old modem port). *In HiveMind:* COM1 = logs, COM2 = the VM-to-VM
  mesh, COM3 = the AI accelerator link.
- **PS/2** — the old protocol for keyboard and mouse, via the "8042" controller at I/O
  ports 0x60/0x64. *In HiveMind:* our keyboard and mouse drivers.
- **ATA / IDE** — the classic protocol for talking to hard disks.
- **PIO (Programmed I/O)** — moving disk data by having the CPU read/write it word by
  word (vs. letting the disk do it via DMA). *In HiveMind:* our disk driver is PIO.
- **LBA / sector** — a disk is a numbered array of 512-byte **sectors**; **LBA** is
  "Logical Block Addressing," i.e., addressing them by number.
- **IDENTIFY** — the ATA command that reports a drive's info (and, we learned, must be
  fully drained or it corrupts the next read).
- **RTC (Real-Time Clock)** — the battery-backed chip that keeps the date/time.

## 6. HiveMind core concepts

- **Hive** — the whole shared memory system: the graph of memory nodes plus the
  agents and signals over it. The top-level "brain."
- **Blob** — the smallest unit of memory: one key-value entry (e.g. `temp = 90`).
- **Memory node (Memory)** — a named container of blobs; a "brain region." Nodes
  connect to each other with edges.
- **Edge** — a directed link between two memory nodes, with a type:
  **Sync** (keep in step), **Signal** (one-way event), **Mirror** (replica for
  redundancy), **Dependency** (one relies on another).
- **DAG (Directed Acyclic Graph)** — a graph with no cycles (you can't loop back).
  *In HiveMind:* memory edges form a DAG; the kernel rejects edges that would create
  a loop.
- **Subscription** — a node "watching" another node for changes. *In HiveMind:* a
  `Sync`/`Signal` edge auto-subscribes the target to the source.
- **Signal** — a broadcast event sent from a node to its subscribers.
- **Agent** — an autonomous entity attached to a memory node that watches blobs and
  acts (writes blobs, signals). *In HiveMind:* rule-based in the kernel; it can also
  consult the LLM.
- **Rule** — an agent's *if-condition-then-action*: e.g. *if `temp` > 80, write
  `alert = HIGH`.* Conditions: `gt:N`, `lt:N`, `eq:S`, `any`.
- **Edge-triggered** — firing once when a condition *becomes* true (a false→true
  transition), not every moment it stays true. *In HiveMind:* how agents fire now.
- **Tick** — one step of the system clock; agents are evaluated on a tick.
- **Audit log** — a record of every action an agent took (when, what). Supports the
  "auditable by construction" security claim.
- **Mesh** — the VM-to-VM link over COM2; instances share memory by sending
  `HMSG|node|key|value` lines. *In HiveMind:* how two OSes form a swarm.
- **Swarm** — many instances acting together via shared memory.
- **Persistence / autosave** — saving state to disk so it survives a reboot. *In
  HiveMind:* a debounced auto-save + auto-load on boot.
- **VFS (Virtual File System)** — the in-memory files/directories (`ls`, `cat`, …).
- **UUID (v4)** — a random 128-bit identifier like `a2d7…`. *In HiveMind:* a fresh one
  each boot, so every instance is uniquely and unpredictably identifiable.

## 7. Agents & AI

- **LLM (Large Language Model)** — an AI model (like GPT/Claude/Llama) that generates
  text. Too big to run inside a kernel.
- **Inference** — running a model to get an output. *In HiveMind:* offloaded to the
  host, like offloading to a GPU.
- **Parameters (1B, 3B…)** — a model's size; more = smarter but heavier. *In HiveMind:*
  we use tiny 1B models so they run on a laptop CPU.
- **Ollama** — a free tool that downloads and runs local LLMs and exposes them over a
  simple HTTP API. *In HiveMind:* our bridge calls Ollama.
- **Prompt / system prompt / context** — the input you give a model. The **system
  prompt** sets its role; **context** is the situation (here: a node's blobs).
- **Coprocessor / accelerator** — a helper chip the CPU offloads heavy work to (GPU,
  NPU). *In HiveMind:* the LLM is treated as a reasoning accelerator reached over a
  device (COM3).
- **Two-tier cognition** — our design: a **fast reflex tier** (in-kernel rules,
  instant) plus a **slow deliberation tier** (the offloaded LLM). Like System-1/
  System-2 (fast/slow) thinking, at the OS level.
- **RAG (Retrieval-Augmented Generation)** — feeding a model relevant facts so it
  doesn't hallucinate. Related to how we pass blob state as context.
- **Agent orchestration / planner-executor** — coordinating multiple agents; a
  **planner** splits a task into steps, **executors** run them. (LLMCompiler's model.)
- **MemGPT** — a research system that gives one LLM an OS-like memory hierarchy
  (a metaphor, in userspace). HiveMind is the *actual* OS version of that idea.

## 8. Distributed systems & security (the research angle)

- **Virtualization / hypervisor** — running a virtual computer; the **hypervisor** is
  the layer that makes it possible.
- **VM (Virtual Machine)** — a whole virtual computer. Strong isolation but heavier
  than a container.
- **Container (Docker)** — a lighter form of isolation that *shares the host kernel*;
  weaker separation than a VM.
- **microVM (Firecracker)** — a stripped-down, fast-booting VM (powers AWS Lambda).
  The closest real comparable to HiveMind — but it still boots Linux inside.
- **gVisor / Kata** — sandboxing tech between containers and VMs.
- **Isolation** — how well one workload is prevented from affecting/reading another.
  *In HiveMind:* hardware-level (VM boundary) per agent.
- **TCB (Trusted Computing Base)** — the total amount of code that must be trusted/
  audited for security. Smaller = safer. *In HiveMind:* tiny (our kernel), vs Linux's
  ~30M lines — a key selling point.
- **Zero-trust** — assume nothing is trusted by default; verify and contain
  everything. *In HiveMind:* the north-star framing.
- **Provenance** — a traceable record of where data came from / who changed it.
- **Single-system-image / process migration** — making many machines look like one;
  moving a running process between machines. Related to the "agent placement
  scheduler" idea. (MOSIX, Plan 9 are classic examples.)
- **Actor model / Erlang BEAM / Ray** — programming models where the unit of work is a
  small message-driven "actor" the runtime can freely place and move. The *right* way
  to do "split load across the kernel" (move agents, don't split a thread).
- **Capability-based security** — granting each component only the specific
  permissions it needs. A natural future direction for agent isolation.

## 9. Host platform (kernel/vos/observer)

- **Tokio** — Rust's async runtime (lets many tasks run concurrently). Used by the
  host crates.
- **async / await** — a way to write concurrent code that waits without blocking.
- **HTTP / REST API** — the standard request/response way programs talk over a
  network. *In HiveMind:* the host kernel exposes `/hive/...` endpoints.
- **axum** — the Rust web framework serving that API.
- **serde / JSON** — serialization: turning data structures into text (JSON) and back.
- **Snapshot** — a serialized copy of the whole hive state, which the observer polls.
- **egui / eframe** — a Rust GUI toolkit; the **observer** window is built with it.
- **Observer** — the desktop app that draws the live hive as a graph.
- **Force-directed layout** — a graph-drawing method where nodes repel and edges pull,
  so the layout self-organizes.
- **VM manager (vos)** — the host program that starts/stops QEMU VMs and represents
  each as a memory node.
- **Mesh bridge** — host code that syncs a VM's state into the hive.
- **VNC** — a protocol for viewing a VM's screen remotely.

## 10. Benchmarking & evaluation (research)

- **Benchmark** — a repeatable experiment that measures something (speed, memory,
  isolation) so you can compare systems fairly.
- **Baseline** — the existing systems you compare against (containers, Firecracker…).
- **SLO (Service Level Objective)** — a target like "99% of requests under 100 ms."
  *In HiveMind:* the Jia-style scheduler angle.
- **Throughput / latency** — how *much* per second / how *long* each one takes.
- **Red-team suite** — deliberate attack tests to prove isolation (can agent A read
  agent B's secrets?).
- **Reproducibility** — others can re-run your experiment and get the same result.
  A core value for Percy Liang / HELM.
- **HELM** — Stanford's rigorous, reproducible LLM-evaluation framework.

## 11. Tools & environment

- **QEMU** — the emulator that runs our OS as a virtual PC.
- **TCG** — QEMU's pure-software CPU emulation (correct but slow).
- **WHPX (Windows Hypervisor Platform)** — hardware acceleration on Windows that runs
  the guest on the real CPU (much faster). *In HiveMind:* `run-os.ps1` uses it.
- **vCPU / -smp** — the number of virtual CPU cores given to a VM.
- **Monitor (QEMU)** — QEMU's control console (used in testing to dump the screen with
  `pmemsave` and inject keys with `sendkey`).
- **WDAC (Windows Application Control)** — a Windows security policy that can block
  newly-built executables (the `os error 4551` we hit rebuilding the host crates).
- **Git / commit / branch / push / `main` / PR** — version control: a **commit** is a
  saved change; **push** uploads to GitHub; **`main`** is the primary branch; a **PR
  (pull request)** proposes merging one branch into another.
