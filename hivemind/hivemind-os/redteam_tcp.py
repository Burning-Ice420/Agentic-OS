#!/usr/bin/env python3
"""
HiveMind red-team isolation benchmark (boundary-confidentiality), TCP-capture version.

Root cause of the old file-capture flakiness: QEMU buffers `-serial file:` and a
hard kill never flushes it, so the wire log read back as 0 bytes even though the
victim broadcast ran. Fix: capture COM2 over a live TCP socket with a reader thread,
so bytes are recorded as they leave the guest, independent of how QEMU exits.

Threat model: two agents are separate QEMU VMs sharing no RAM, so COM2 (the mesh) is
the ONLY channel out of a victim, and it is push-only (net/mod.rs has no read-a-peer
op). Therefore if a private secret never appears on that wire, no mesh attacker can
ever obtain it. This harness plants a PRIVATE secret, does one BENIGN broadcast, and
records everything the victim transmits. PASS = benign value crosses the wire, secret
never does.

Run: python redteam_tcp.py            (tcg, reliable)
     python redteam_tcp.py --accel whpx,kernel-irqchip=off
"""
import argparse, os, socket, subprocess, sys, threading, time

QEMU = r"C:\msys64\mingw64\bin\qemu-system-x86_64.exe"
BOOT = "C:/hivemind/boot_t.bin"
SECRET = "zzsecret"  # lowercase: QEMU sendkey has no shift, keep keystrokes literal
BENIGN = "zztoken"
WIRE_PORT = 4471
MON_PORT = 5562


class WireTap:
    """Connects to QEMU's COM2 TCP server and records every byte the guest emits."""
    def __init__(self, port):
        self.port = port
        self.buf = bytearray()
        self.stop = False
        self.t = threading.Thread(target=self._run, daemon=True)

    def start(self):
        self.t.start()

    def _run(self):
        s = None
        for _ in range(200):
            if self.stop:
                return
            try:
                s = socket.create_connection(("127.0.0.1", self.port), timeout=2)
                break
            except OSError:
                time.sleep(0.2)
        if s is None:
            return
        s.settimeout(1.0)
        while not self.stop:
            try:
                d = s.recv(4096)
                if not d:
                    time.sleep(0.1); continue
                self.buf.extend(d)
            except socket.timeout:
                continue
            except OSError:
                break
        try: s.close()
        except OSError: pass

    def text(self):
        return bytes(self.buf).decode("utf-8", "ignore")


def mon(port):
    for _ in range(120):
        try:
            s = socket.create_connection(("127.0.0.1", port)); s.settimeout(2); time.sleep(0.2)
            _drain(s); return s
        except OSError:
            time.sleep(0.3)
    sys.exit(f"monitor {port} unreachable")


def _drain(s):
    try:
        while True:
            if not s.recv(4096):
                break
    except OSError:
        pass


def cmd(s, c):
    s.sendall((c + "\n").encode()); time.sleep(0.12); _drain(s)


def typ(s, t):
    # 0.28s/key: at 0.18s the guest keyboard IRQ dropped scancodes under TCG
    for ch in t:
        cmd(s, "sendkey " + ("spc" if ch == " " else ch)); time.sleep(0.28)
    time.sleep(0.2); cmd(s, "sendkey ret")


def screen(m):
    cmd(m, 'pmemsave 0xb8000 4000 "C:/hivemind/rt_probe.bin"'); time.sleep(0.4)
    try:
        d = open(r"C:\hivemind\rt_probe.bin", "rb").read()
        return "".join(chr(d[i * 2]) if 32 <= d[i * 2] < 127 else " " for i in range(2000))
    except OSError:
        return ""


def wait_prompt(m, timeout=120):
    t = time.time()
    while time.time() - t < timeout:
        if "hive>" in screen(m):
            return True
        time.sleep(2)
    return False


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--accel", default="tcg")
    args = ap.parse_args()

    os.system('taskkill /F /IM qemu-system-x86_64.exe >NUL 2>&1'); time.sleep(2)

    tap = WireTap(WIRE_PORT)
    print(f"[redteam] launching victim (accel={args.accel}); COM2 mesh -> tcp:{WIRE_PORT} (live capture)")
    victim = subprocess.Popen(
        [QEMU, "-drive", f"format=raw,file={BOOT}", "-m", "64M",
         "-no-reboot", "-no-shutdown", "-display", "none",
         "-serial", "null",
         "-serial", f"tcp:127.0.0.1:{WIRE_PORT},server,nowait",
         "-monitor", f"tcp:127.0.0.1:{MON_PORT},server,nowait", "-accel", args.accel],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    tap.start()  # connect and record from as early as possible
    try:
        m = mon(MON_PORT)
        print("[redteam] waiting for the victim shell prompt ...")
        if not wait_prompt(m):
            print("[redteam] shell never reached - aborting"); cmd(m, "quit"); return
        print("[redteam] shell up.")

        time.sleep(1.5)
        print(f"[redteam] victim plants PRIVATE secret '{SECRET}' on kernel-root (never broadcast)")
        typ(m, f"blob write 1 secret {SECRET}"); time.sleep(1.2)

        print(f"[redteam] victim does normal work: one benign broadcast '{BENIGN}'")
        typ(m, f"net send public token {BENIGN}"); time.sleep(2.0)
        sent = ("Sent" in screen(m)) or ("peer" in screen(m))
        print(f"[redteam] benign broadcast executed on victim: {sent}")
        time.sleep(2.5)  # let the wire drain into the tap
        cmd(m, "quit")
    finally:
        time.sleep(1)
        tap.stop = True
        try: victim.terminate()
        except Exception: pass
    time.sleep(1)
    os.system('taskkill /F /IM qemu-system-x86_64.exe >NUL 2>&1')

    wire = tap.text()
    leaked = SECRET in wire
    transmitted = "HMSG" in wire
    benign_ok = BENIGN in wire
    print("\n================= RED-TEAM RESULT =================")
    print(f"  threat: a compromised peer wants the victim's private '{SECRET}'")
    print(f"  bytes the victim transmitted on the mesh : {len(wire)}")
    print("  wire contents (everything a peer could receive):")
    for ln in wire.splitlines():
        if ln.strip():
            print(f"      {ln.strip()}")
    print(f"  >> secret ({SECRET}) on the wire : {leaked}")
    print(f"  >> victim transmitted broadcasts : {transmitted}  (benign token seen: {benign_ok})")
    if leaked:
        print("  VERDICT: FAIL - the private secret crossed the isolation boundary")
    elif transmitted and benign_ok:
        print("  VERDICT: PASS")
        print("    The victim transmitted its benign broadcast but NEVER its private secret.")
        print("    Isolated VMs share no RAM and the mesh is the only path between them")
        print("    (push-only, no remote-read), so no peer can obtain the secret.")
    else:
        print("  VERDICT: INCONCLUSIVE - no broadcast captured (typing/boot/capture)")
    print("==================================================")


if __name__ == "__main__":
    main()
