"""Energy measurement via the Windows Energy Meter Interface (EMI), which exposes
the CPU's RAPL counters.

Calibrated on this machine (research-docs/ENERGY_METHOD.md):
  \\Energy Meter(<rail>)\\Energy  cumulative picowatt-hours  (1 pWh = 3.6e-9 J)
  \\Energy Meter(<rail>)\\Time    milliseconds
Rails: rapl_package0_pkg (whole SoC package: cores + uncore + iGPU), _pp0 (cores),
_pp1 (iGPU), _dram (reads 0 here: not reported by this platform).

What is NOT measured: the rest of the laptop (display, storage, wall losses).
Results are CPU-package energy, not wall energy.
"""
from __future__ import annotations

import json
import subprocess
import time

RAILS = ("rapl_package0_pkg", "rapl_package0_pp0", "rapl_package0_pp1", "rapl_package0_dram")
PWH_TO_J = 3.6e-9

# One persistent PowerShell process answers reads, so a read does not pay a process
# start (which would itself burn energy inside the measured window).
_PS_LOOP = r"""
$paths = @(%s)
while ($true) {
  $line = [Console]::In.ReadLine()
  if ($line -eq $null -or $line -eq 'quit') { break }
  $s = (Get-Counter -Counter $paths).CounterSamples
  $o = @{}
  foreach ($x in $s) { $o[$x.Path.Split('\')[-2] + '|' + $x.Path.Split('\')[-1]] = $x.RawValue }
  [Console]::Out.WriteLine(($o | ConvertTo-Json -Compress))
  [Console]::Out.Flush()
}
"""


class EnergyMeter:
    def __init__(self):
        paths = []
        for r in RAILS:
            paths += [f"'\\Energy Meter({r})\\Energy'", f"'\\Energy Meter({r})\\Time'"]
        script = _PS_LOOP % ",".join(paths)
        self.p = subprocess.Popen(["powershell", "-NoProfile", "-Command", script],
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                  text=True, bufsize=1)

    def read(self) -> dict:
        self.p.stdin.write("read\n")
        self.p.stdin.flush()
        raw = json.loads(self.p.stdout.readline())
        out = {}
        for k, v in raw.items():
            inst, ctr = k.split("|")
            inst = inst.split("(")[-1].rstrip(")")
            out.setdefault(inst, {})[ctr] = int(v)
        out["_host_t"] = time.perf_counter()
        return out

    def close(self):
        try:
            self.p.stdin.write("quit\n")
            self.p.stdin.flush()
        except OSError:
            pass
        self.p.wait(timeout=10)


def detect() -> dict:
    """Which energy sources this machine exposes."""
    res = {"emi_rapl": False, "rails_nonzero": [], "battery_discharge_rate": None}
    try:
        m = EnergyMeter()
        a = m.read(); time.sleep(1.0); b = m.read(); m.close()
        res["emi_rapl"] = True
        res["rails_nonzero"] = [r for r in RAILS if r in a and b[r]["energy"] > a[r]["energy"]]
    except Exception as e:  # noqa: BLE001
        res["error"] = repr(e)
    return res


def window(a: dict, b: dict) -> dict:
    """Energy (J) and duration (s) per rail between two reads."""
    out = {}
    for r in RAILS:
        if r in a and r in b:
            dt = (b[r]["time"] - a[r]["time"]) / 1000.0
            out[r] = {"joules": (b[r]["energy"] - a[r]["energy"]) * PWH_TO_J, "seconds": dt}
    return out


def idle_baseline(meter: EnergyMeter, seconds: float = 20.0, reps: int = 3) -> dict:
    """Mean package power (W) with nothing running from this harness."""
    ws = []
    for _ in range(reps):
        a = meter.read(); time.sleep(seconds); b = meter.read()
        w = window(a, b)["rapl_package0_pkg"]
        ws.append(w["joules"] / w["seconds"])
    m = sum(ws) / len(ws)
    sd = (sum((x - m) ** 2 for x in ws) / (len(ws) - 1)) ** 0.5 if len(ws) > 1 else 0.0
    return {"watts_mean": m, "watts_sd": sd, "samples": ws, "seconds_each": seconds}


def carbon_grams(joules: float, intensity_g_per_kwh: float | None, source: str | None, timestamp: str | None):
    """Only computed when the caller supplies a time-stamped intensity and its source."""
    if intensity_g_per_kwh is None or not source or not timestamp:
        return {"computed": False, "reason": "no time- and location-specific carbon intensity supplied"}
    return {"computed": True, "grams_co2e": joules / 3.6e6 * intensity_g_per_kwh,
            "intensity_g_per_kwh": intensity_g_per_kwh, "source": source, "timestamp": timestamp}
