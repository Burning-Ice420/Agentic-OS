"""Drive one bare-metal HiveMind guest under QEMU for experiments.

Boot discipline: do not touch the QEMU monitor during early boot; wait, then check
for the prompt once. Monitor file paths must use forward slashes (the monitor treats
backslash as an escape; an earlier "boot hang" was this harness bug, not the guest). A guest that does not reach the prompt is recorded as a failed run, never
silently retried inside a measurement.
"""
from __future__ import annotations

import os
import socket
import subprocess
import time

QEMU = r"C:\msys64\mingw64\bin\qemu-system-x86_64.exe"
QEMU_IMG = r"C:\msys64\mingw64\bin\qemu-img.exe"
WORK = r"C:\hivemind"

KEYMAP = {" ": "spc", "-": "minus", ":": "shift-semicolon", ",": "comma", ".": "dot",
          "/": "slash", "_": "shift-minus", "=": "equal", ";": "semicolon", "*": "shift-8"}


def _key(ch: str) -> str:
    if ch in KEYMAP:
        return KEYMAP[ch]
    if ch.isupper():
        return "shift-" + ch.lower()
    return ch


class Guest:
    def __init__(self, name: str, boot: str, accel: str = "whpx,kernel-irqchip=off", mem: str = "64M",
                 monitor_port: int = 5590, com2: str | None = None, com3: str | None = None,
                 data_disk_mb: int | None = None):
        self.name = name
        self.log = os.path.join(WORK, f"{name}.log")
        self.port = monitor_port
        self.boot = boot
        self.data = os.path.join(WORK, f"{name}-data.img") if data_disk_mb else None
        args = [QEMU, "-drive", f"format=raw,file={boot}"]
        if self.data:
            if os.path.exists(self.data):
                os.remove(self.data)
            subprocess.run([QEMU_IMG, "create", "-f", "raw", self.data, f"{data_disk_mb}M"], capture_output=True)
            args += ["-drive", f"file={self.data},format=raw,if=ide,index=1"]
        if os.path.exists(self.log):
            os.remove(self.log)
        args += ["-m", mem, "-no-reboot", "-no-shutdown", "-display", "none",
                 "-serial", f"file:{self.log}",
                 "-serial", com2 or "null"]
        if com3:
            args += ["-serial", com3]
        args += ["-monitor", f"tcp:127.0.0.1:{monitor_port},server,nowait", "-accel", accel]
        self.args = args
        self.proc = None
        self.sock = None

    def start(self):
        self.proc = subprocess.Popen(self.args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        return self

    def _mon(self):
        if self.sock is None:
            for _ in range(100):
                try:
                    self.sock = socket.create_connection(("127.0.0.1", self.port), timeout=3)
                    break
                except OSError:
                    time.sleep(0.3)
            if self.sock is None:
                raise RuntimeError("monitor unreachable")
            self.sock.settimeout(1)
            time.sleep(0.3)
            self._drain()
        return self.sock

    def _drain(self):
        try:
            while self.sock.recv(4096):
                pass
        except OSError:
            pass

    def cmd(self, c: str):
        s = self._mon()
        s.sendall((c + "\n").encode())
        time.sleep(0.12)
        self._drain()

    def screen(self) -> str:
        path = os.path.join(WORK, f"{self.name}-vga.bin")
        # the QEMU monitor treats backslash as an escape: always pass forward slashes
        self.cmd(f'pmemsave 0xb8000 4000 "{path.replace(chr(92), "/")}"')
        time.sleep(0.6)
        try:
            d = open(path, "rb").read()
        except OSError:
            return ""
        rows = []
        for r in range(25):
            row = "".join(chr(d[(r * 80 + c) * 2]) if 32 <= d[(r * 80 + c) * 2] < 127 else " " for c in range(80))
            rows.append(row.rstrip())
        return "\n".join(rows)

    def wait_ready(self, settle: float = 55.0) -> bool:
        time.sleep(settle)
        return "hive>" in self.screen()

    def type(self, text: str, after: float = 0.6, per_key: float = 0.28):
        for ch in text:
            self.cmd("sendkey " + _key(ch))
            time.sleep(per_key)
        time.sleep(0.2)
        self.cmd("sendkey ret")
        time.sleep(after)

    def stop(self):
        try:
            self.cmd("quit")
        except Exception:  # noqa: BLE001
            pass
        time.sleep(1.5)
        if self.proc and self.proc.poll() is None:
            self.proc.kill()
        if self.sock:
            try:
                self.sock.close()
            except OSError:
                pass

    def read_log(self) -> list[str]:
        try:
            return open(self.log, encoding="utf-8", errors="replace").read().splitlines()
        except OSError:
            return []


def kill_all_qemu():
    os.system("taskkill /F /IM qemu-system-x86_64.exe >NUL 2>&1")
    time.sleep(2)
