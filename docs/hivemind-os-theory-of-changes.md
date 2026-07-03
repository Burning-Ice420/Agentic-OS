# HiveMind OS — Theory of Changes

This document explains the **theory** behind each change made when the OS was
reworked to be interrupt-driven and gained persistence, scrollback, a mouse
desktop, per-boot identity, hardware acceleration, and a host-side CLI.

It assumes you have read `hivemind-os-knowledge-dump.md`. Where that doc explains
what the OS *is*, this one explains *why* each new piece works the way it does —
so you understand the concepts, not just the code.

---

## 1. Interrupt-driven input (why the keyboard stopped "breaking")

### The theory: polling vs interrupts
A CPU can find out about hardware events two ways:

- **Polling** — you loop and repeatedly ask "any key yet? any key yet?". Simple,
  but if your loop is busy doing something else (e.g. writing to a slow disk),
  nobody is asking, and keypresses are lost.
- **Interrupts** — the hardware taps the CPU on the shoulder. The CPU stops
  whatever it's doing, runs a small **interrupt handler**, then resumes. Input is
  never missed because the tap happens no matter what the main code is doing.

### What was wrong
The OS registered interrupt handlers but **never executed `sti`** (the instruction
that enables interrupts). So the timer and keyboard interrupts never fired. The
keyboard only worked because the shell *polled* port `0x60` each loop iteration.
Any command that held the loop for a while — especially `save`/`load`, which
busy-wait on the disk — stopped the polling, and keys typed in that window were
dropped from the tiny 16-byte keyboard buffer. It looked like "the filesystem
broke the keyboard".

### The fix
Enable interrupts (`x86_64::instructions::interrupts::enable()`), make the
keyboard fully interrupt-driven (IRQ1 pushes characters into a ring buffer), and
remove the polling. The shell now `hlt`s (halts the CPU) until the next
interrupt, which keeps input responsive and the CPU cool. Enabling interrupts is
what exposed the four deeper bugs below.

---

## 2. Atomic counter vs Mutex (the deadlock)

`TICKS` (a counter the timer bumps every tick) was a `Mutex<u64>`. A **spinlock
mutex** on a single CPU is dangerous inside interrupts: if the main code holds
the lock when the timer interrupt fires, the timer handler tries to take the same
lock, spins forever waiting for a lock that can never be released (the main code
is frozen mid-interrupt) → **deadlock**.

The fix is an `AtomicU64`. Atomic operations are a single indivisible CPU
instruction (`lock xadd`) — there is no lock to hold, so an interrupt can never
catch it "half done". Rule of thumb: **data shared with an interrupt handler
should be lock-free (atomic) or accessed only with interrupts disabled.**

---

## 3. SSE, the `x86-interrupt` ABI, and soft-float

This is the subtle one. When you enable interrupts and the first timer IRQ fires,
the CPU jumped to the handler and immediately triple-faulted.

### The theory
- **SSE** is the set of x86 instructions that use the 128-bit `XMM` registers
  (used for floating point and bulk memory moves like `movaps`).
- An **`extern "x86-interrupt"` handler** must preserve *every* register it might
  clobber, because it can interrupt code at any instant. The compiler therefore
  emits a prologue that saves all the XMM registers with `movaps` — *even if your
  handler never touches floats*.
- `movaps` only works if SSE is enabled in the CPU (`CR0`/`CR4` bits). If SSE is
  off, `movaps` raises **#UD (invalid opcode)**. The ordinary boot code happened
  never to emit an XMM instruction, so it ran fine — but every interrupt handler
  prologue did, so the first interrupt faulted.

### The fix
Build the kernel as a **soft-float target**: SSE disabled, floating point done in
software, so the compiler never emits XMM instructions anywhere — including
interrupt prologues. Modern rustc requires you to declare this explicitly:

```json
"rustc-abi": "x86-softfloat",
"features" : "-mmx,-sse,+soft-float"
```

(`-sse` alone fails because `core` returns `f64` in an XMM register per the normal
ABI; you need the matching soft-float ABI to change that calling convention.)

---

## 4. The IDT, PIC masking, and spurious interrupts

Even after soft-float, the first interrupt double-faulted — this time because of
an interrupt the OS didn't expect.

### The theory
- The **IDT** (Interrupt Descriptor Table) maps each of 256 interrupt *vectors*
  to a handler. If a vector fires but its IDT slot is empty (not present), the CPU
  raises a fault; if *that* can't be delivered either, you get a **double fault**.
- The **PIC** (Programmable Interrupt Controller) routes 16 hardware IRQ lines to
  vectors 32–47. Each line can be **masked** (ignored) or unmasked. A "mask" byte
  has one bit per IRQ; a `0` bit means "deliver this IRQ".
- Devices assert IRQs whether or not you have a handler. The disk (ATA) uses
  **IRQ14 → vector 46**. Its leftover interrupt from boot-time disk activity fired
  into an empty IDT slot → double fault.

### The fix (two layers of defence)
1. **Mask what you don't use.** Set the PIC masks to enable only timer (IRQ0),
   keyboard (IRQ1), the slave cascade (IRQ2) and, later, the mouse (IRQ12). The
   ATA driver is fully polled, so IRQ14 stays masked.
2. **Catch anything unexpected.** Install a default "spurious IRQ" handler on all
   of vectors 32–47 that simply acknowledges the PIC (sends EOI) and returns, plus
   real exception handlers (#GP, #UD, #NP, …) so a stray fault reports instead of
   silently triple-faulting.

**EOI** ("End Of Interrupt") is the signal the handler sends back to the PIC to
say "done, you may deliver the next one". Forget it and the PIC wedges.

---

## 5. ATA PIO reads: draining IDENTIFY + the select delay

Persistence saved correctly but wouldn't restore after a reboot.

### The theory
- The disk driver talks to the drive with **PIO** (Programmed I/O): you write LBA
  and a command to I/O ports, the drive raises `DRQ` (Data ReQuest) when a 512-byte
  sector is ready, and you read 256 16-bit words from the data port.
- `IDENTIFY` is the "tell me about yourself" command. It *also* returns 512 bytes
  via the same DRQ/data-port mechanism. The old code detected the drive by waiting
  for `DRQ` but **never read those 512 bytes**. The drive was left with a full data
  buffer, and the first *real* read got confused by the stale data — reads only
  started working after a write happened to clear the buffer (hence "worked in the
  same session, failed after reboot").
- Selecting a drive (master vs slave) needs a short **~400 ns settle delay** before
  the command registers are valid; without it the first read on a cold drive can
  return zeros.

### The fix
Drain the 256-word IDENTIFY block after detection (and grab words 60–61 while
we're there — they hold the drive's total sector count, used by `sysinfo`), and
add the 400 ns settle after selecting the drive in both read and write.

---

## 6. VGA scrollback (PageUp / PageDown)

### The theory
VGA text mode is a fixed 80×25 grid of memory at `0xb8000`; there is no built-in
history. To scroll, you keep your own **scrollback buffer**: a ring of the last N
logical rows. What you see on screen is a **viewport** — a 25-row window into that
ring. A `view` offset says how far up from the live bottom you're looking.

- New output always snaps the viewport back to the bottom (standard terminal
  behaviour) and is written cheaply (only the bottom row + a hardware scroll).
- PageUp/PageDown move `view`; when scrolled, the whole visible window is re-blitted
  from the ring.

The ring lives in `.bss` (zero-initialised static memory), not on the stack — a
200×80 array is ~32 KB and would overflow the small boot stack if constructed
there.

Special keys (PageUp, arrows, …) aren't ASCII, so they're funnelled through the
same character queue using otherwise-unused control codes and decoded by the
shell.

---

## 7. Persistence: dirty-flag debounce

Writing the whole state to disk on *every* command would make the shell laggy
(disk writes are slow). Instead we use a **debounced dirty flag**:

- Any state-changing command calls `mark_dirty()` and records the current tick.
- The main loop calls `autosave_tick()`; once the state has been dirty and *quiet*
  for ~3 seconds (54 timer ticks) it flushes once. `halt` also flushes.

On disk, sector 0 holds a **magic header** (`HIVEMIND\x01`) so boot can tell "this
disk has saved state" from "blank disk". The hive and filesystem are serialised as
simple text records (`MEM|id|name`, `BLOB|id|key|value`, …) — easy to debug by
hexdumping the image.

---

## 8. PS/2 mouse (IRQ12)

### The theory
The PS/2 mouse hangs off the same 8042 controller as the keyboard, as the
"auxiliary device" on **IRQ12**. After you enable it (controller command `0xA8`,
enable IRQ12 in the config byte, then `0xF4` "enable data reporting"), it streams
**3-byte packets**: `[flags, dx, dy]`. `flags` carries the button states, movement
sign bits, and overflow bits; bit 3 is always 1, which lets you re-synchronise if
you ever lose alignment.

Two gotchas handled:
- The `0xF4` command replies with an **ACK byte `0xFA`**. If that ACK leaks into the
  packet stream it desyncs everything (it looks like a header with both overflow
  bits set, so movement gets discarded). We drain pending bytes after init and
  reject `0xFA`/`0xAA`/`0xFE` as packet headers.
- Movement is **relative**; we accumulate it into a sub-cell position and clamp to
  the 80×25 grid. Screen Y is inverted relative to the mouse's Y sign.

---

## 9. Text-mode desktop (a tiny window manager)

Concepts a window manager needs, all done with characters:

- **Windows** are rectangles with a title bar, a close box, and content. Each has a
  position and size.
- **Z-order** — a list of open windows; the last is on top and has focus. Clicking
  a window "raises" it (moves it to the end of the list). Drawing bottom-to-top
  gives correct overlap.
- **Hit-testing** — on a click, walk the windows top-first and see which rectangle
  contains the cursor cell; that window wins. Icons and the taskbar are tested too.
- **Dragging** — on title-bar press, remember the offset between the cursor and the
  window corner; while the button stays down, move the window so that offset is
  preserved.
- **Cursor** — drawn last each frame by *inverting* the colour of the cell under the
  pointer, so it's always visible over any content without needing to save/restore
  what was there.

---

## 10. Per-boot instance UUID (identity + a little security)

### The theory
Each boot generates a fresh **UUID v4** (a 122-bit random identifier in the
`xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx` shape). "Version 4" just means "randomly
generated"; the `4` and the variant bits are fixed by the RFC-4122 standard.

Randomness on a bare-metal machine has no OS RNG, so we mix several
**entropy sources**:

- `rdtsc` — the CPU cycle counter. It reads a huge, fast-moving number whose exact
  value at boot depends on unpredictable timing.
- The **RTC** wall-clock time.
- A rolling per-init salt.

These are combined and run through **SplitMix64**, a small high-quality bit mixer,
to produce the 128 bits. Because the identifier changes every run and can't be
predicted from the outside, it's a lightweight anti-replay / fingerprinting
measure: a captured UUID is useless on the next boot. The kernel prints it to COM1
at boot so the host CLI can map a serial log back to the instance that produced it.

---

## 11. Hardware acceleration (WHPX)

By default QEMU **emulates** every guest instruction in software (TCG) — correct
but slow. **WHPX** (Windows Hypervisor Platform) instead runs the guest's
instructions **directly on your real CPU** using virtualization extensions, which
is dramatically faster (the boot animation that crawled under TCG finishes in a
second or two).

WHPX only accelerates the CPU; QEMU still emulates the chipset (PIC, PIT, PS/2,
ATA), so our drivers are unchanged. WHPX cannot use an in-kernel interrupt
controller, so we pass `kernel-irqchip=off`. If a machine lacks the Windows
Hypervisor Platform feature, `-Accel tcg` falls back to software emulation.

---

## 12. Reporting resources (the `sysinfo` command)

- **RAM** — the bootloader hands the kernel a **memory map** listing every physical
  region and its type. We sum the `Usable` regions (you must *not* just take the
  highest address, because the map also contains a huge high-address
  physical-memory-mapping region → that would report a terabyte).
- **CPU** — `CPUID` leaves `0x8000_0002..0x8000_0004` return the CPU brand string.
  The kernel is single-core, so it reports 1 even if QEMU allocates more vCPUs.
- **Disk** — words 60–61 of the ATA IDENTIFY block give the LBA28 sector count;
  ×512 is the capacity.

---

## 13. Host-side CLI and the instance registry

`run-os.ps1` writes a small JSON **manifest** per launched VM under
`C:\hivemind\instances\` (pid, serial-log path, RAM/CPU/disk, launch time).
`hive-cli.ps1` reads those manifests, checks which processes are still alive,
greps each serial log for the `instance-uuid=…` line, and presents `list` / `info`
/ `uuid` / `connect` / `stop`. Because it runs in Windows Terminal / PowerShell you
get native scroll-back for long output; `connect` live-follows an instance's
console with `Get-Content -Wait`.

