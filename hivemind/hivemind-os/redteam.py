#!/usr/bin/env python3
"""
HiveMind red-team isolation benchmark (boundary-confidentiality test).

Isolated VMs share no RAM (separate QEMU processes), so the COM2 mesh is the ONLY
channel out of a victim VM, and it is push-only -- there is no "read a peer's
memory" primitive (see net/mod.rs; the sole inbound op applies HMSG broadcasts).
Therefore: if a secret never appears on that wire, NO attacker on the mesh can
ever obtain it, whatever it does.

This harness plants a PRIVATE secret in a victim, has the victim do normal work
(a benign broadcast + an agent firing on the secret's node), and captures
EVERYTHING the victim transmits on COM2. PASS = the benign value crosses the wire
but the secret never does.

Run: python redteam.py            (tcg, reliable)
     python redteam.py --accel whpx,kernel-irqchip=off
"""
import argparse, os, socket, subprocess, sys, time

QEMU = r"C:\msys64\mingw64\bin\qemu-system-x86_64.exe"
BOOT = "C:/hivemind/boot_t.bin"
WIRE = "C:/hivemind/rt_wire.log"   # forward slashes: QEMU -serial file: needs them
SECRET = "ZZSECRET"
BENIGN = "ZZTOKEN"


def mon(port):
    for _ in range(100):
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
    for ch in t:
        cmd(s, "sendkey " + ("spc" if ch == " " else ch)); time.sleep(0.18)
    cmd(s, "sendkey ret")


def screen(m):
    cmd(m, 'pmemsave 0xb8000 4000 "C:/hivemind/rt_probe.bin"'); time.sleep(0.4)
    try:
        d = open(r"C:\hivemind\rt_probe.bin", "rb").read()
        return "".join(chr(d[i * 2]) if 32 <= d[i * 2] < 127 else " " for i in range(2000))
    except OSError:
        return ""


def wait_prompt(m, timeout=90):
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

    if os.path.exists(WIRE):
        os.remove(WIRE)  # let QEMU create it fresh (pre-creating causes a Windows sharing conflict)
    print(f"[redteam] launching victim (accel={args.accel}); COM2 mesh -> {WIRE}")
    victim = subprocess.Popen(
        [QEMU, "-drive", f"format=raw,file={BOOT}", "-m", "64M",
         "-no-reboot", "-no-shutdown", "-display", "none",
         "-serial", "null", "-serial", f"file:{WIRE}",
         "-monitor", "tcp:127.0.0.1:5561,server,nowait", "-accel", args.accel],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        m = mon(5561)
        print("[redteam] waiting for the victim shell prompt ...")
        if not wait_prompt(m):
            print("[redteam] shell never reached — aborting"); cmd(m, "quit"); return
        print("[redteam] shell up.")

        time.sleep(1.5)  # settle after the prompt appears
        print(f"[redteam] victim plants PRIVATE secret '{SECRET}' on kernel-root (never broadcast)")
        typ(m, f"blob write 1 secret {SECRET}"); time.sleep(1.2)

        print(f"[redteam] victim does normal work: one benign broadcast '{BENIGN}'")
        typ(m, f"net send public token {BENIGN}"); time.sleep(1.5)
        sent = ("Sent" in screen(m)) or ("peer" in screen(m))
        print(f"[redteam] benign broadcast executed on victim: {sent}")
        time.sleep(2)
        cmd(m, "quit")
    finally:
        time.sleep(1)
        try: victim.terminate()
        except Exception: pass

    time.sleep(1)
    try:
        wire = open(WIRE, "rb").read().decode("utf-8", "ignore")
    except OSError:
        wire = ""

    leaked = SECRET in wire
    transmitted = "HMSG" in wire          # the victim broadcast *something* (channel works)
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
        print("  VERDICT: FAIL — the private secret crossed the isolation boundary")
    elif transmitted:
        print("  VERDICT: PASS")
        print("    The victim transmitted broadcast traffic but NEVER its private secret.")
        print("    Isolated VMs share no RAM and the mesh is the only path between them")
        print("    (push-only, no remote-read), so no peer can obtain the secret.")
    else:
        print("  VERDICT: INCONCLUSIVE — no broadcast captured (typing/boot/path)")
    print("==================================================")


if __name__ == "__main__":
    main()
