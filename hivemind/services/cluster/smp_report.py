"""Build the multi-core results page from an smp_bench record.

  python services/cluster/smp_report.py experiments/results/smp_bench-<stamp>.json [out.html]

Self-contained HTML (inline SVG, no scripts), same look as the cluster report.
"""
from __future__ import annotations

import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from report import PALETTE, bar_chart, esc  # noqa: E402


def lanes(run, w=1100):
    """Per-core timeline of one run from the guest's own job start/end times."""
    cores = run["cores"]
    lane = 22
    h = cores * lane + 30
    L, R = 52, 10
    t1 = max(1, run["ms"])
    sx = lambda t: L + (w - L - R) * t / t1
    s = [f'<svg viewBox="0 0 {w} {h}" role="img">']
    for j in run["jobs_detail"]:
        i = j["core"] - 1
        s.append(f'<rect x="{sx(j["start"]):.1f}" y="{4+i*lane}" width="{max(1.5, sx(j["end"])-sx(j["start"])-1):.1f}" '
                 f'height="{lane-6}" rx="2" fill="{PALETTE[i % len(PALETTE)]}" opacity="0.9"/>')
    for i in range(cores):
        s.append(f'<text x="4" y="{4+i*lane+lane/2+1:.1f}" class="tick">core {i+1}</text>')
    for k in range(0, 6):
        t = t1 * k / 5
        s.append(f'<text x="{sx(t):.1f}" y="{h-6}" class="tick" text-anchor="middle">{t/1000:.1f}s</text>')
    s.append("</svg>")
    return "".join(s)


def build(path: str) -> str:
    rec = json.load(open(path, encoding="utf-8"))
    env = rec["environment"]
    runs = rec["metrics"]["runs"]["value"]
    summ = rec["metrics"]["summary"]["value"]
    corr = rec["metrics"]["correctness"]["value"]
    nw = rec["config"]["compute_cores"]
    sp = [(k, v) for k, v in summ["scaling_speedup"].items()]
    tm = [(k, v / 1000) for k, v in summ["scaling_median_ms"].items()]
    top = sp[-1]
    imb = summ["imbalance_median_ms"]
    imb_x = imb["static"] / max(1, imb["dynamic"])
    def first(prefix):
        return next(r for r in runs if r["label"].startswith(prefix))
    full = first(f"scaling n={nw} rep=0")
    st, dy = first("imbalance static rep=0"), first("imbalance dynamic rep=0")
    kinds = []
    for kind in ("hash", "collatz", "matmul"):
        a = next((r for r in runs if r["label"] == f"kinds {kind} n=1"), None)
        b = next((r for r in runs if r["label"] == f"kinds {kind} n={nw}"), None)
        if a and b:
            kinds.append((kind, a["ms"], b["ms"]))
    host_rows = "".join(
        f"<tr><td>core {i+1}</td><td>{g}%</td><td>{g/100*full['ms']/1000:.2f} s</td><td>{h:.2f} s</td></tr>"
        for i, (g, h) in enumerate(zip(full["guest_busy_pct"], full["host_top_threads_cpu_s"])))
    kind_rows = "".join(f"<tr><td>{k}</td><td>{a/1000:.2f} s</td><td>{b/1000:.2f} s</td><td>{a/max(1,b):.1f}x</td></tr>"
                        for k, a, b in kinds)
    name = os.path.basename(path)
    css = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "report.py"), encoding="utf-8").read()
    style = css[css.index("<style>"):css.index("</style>") + 8].replace("{{", "{").replace("}}", "}")
    return f"""<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>HiveMind Multi-core</title>
<link rel="preconnect" href="https://fonts.googleapis.com"><link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=IBM+Plex+Mono:wght@400;600&family=IBM+Plex+Sans:wght@400;600&family=Space+Grotesk:wght@500;600&display=swap" rel="stylesheet">
</head><body>
{style}
<main>
<header style="display:grid;gap:10px">
  <h1>One HiveMind kernel, {nw + 1} CPU cores</h1>
  <p class="lede">The kernel starts every core in the machine. Core 0 keeps the shell, devices and the memory graph; the kernel places compute jobs on the other {nw}. Measured in one QEMU guest (WHPX) on {esc(env.get('cpu', ''))}; {rec['repetitions']} repetitions where marked, medians shown.</p>
  <p class="src">record: {esc(name)}</p>
</header>

<div class="kpis">
  <div class="kpi"><b>{top[1]:.1f}x</b><span>speedup on {top[0]} cores (48 prime jobs, median of {rec['repetitions']})</span></div>
  <div class="kpi"><b>{top[1]/int(top[0])*100:.0f}%</b><span>parallel efficiency at {top[0]} cores</span></div>
  <div class="kpi"><b>{imb_x:.2f}x</b><span>faster with dynamic placement when one core is loaded</span></div>
  <div class="kpi"><b>{corr['wrong']} wrong</b><span>{corr['checked_vs_host']} of {corr['jobs']} results checked against the host</span></div>
</div>

<section>
  <h2>1. More cores, proportionally faster</h2>
  <p>48 prime-counting jobs (about 0.65 s each on one core). Each core takes the next job when it is free. Dashed marks show perfect linear scaling.</p>
  <div class="two">
    <div class="fig">{bar_chart(sp, "x", ideal=[(k, float(k)) for k, _ in sp])}<p class="src">speedup over 1 core (median)</p></div>
    <div class="fig">{bar_chart(tm, " s", color="var(--accent3)")}<p class="src">time to finish all 48 jobs (median)</p></div>
  </div>
</section>

<section>
  <h2>2. Every core busy, confirmed from outside the guest</h2>
  <p>The guest counts each core's busy cycles; the host measures CPU time of the QEMU threads that run the virtual CPUs. They agree. Run: {nw} cores, {full['ms']/1000:.2f} s.</p>
  <div class="fig">{lanes(full)}<p class="src">which core ran which job, guest timestamps</p></div>
  <div class="fig"><table><tr><th>core</th><th>guest busy</th><th>guest busy time</th><th>host CPU time of a vCPU thread</th></tr>{host_rows}</table>
  <p class="src">host column: the {nw} busiest QEMU threads, sorted, so rows pair by rank, not by core number</p></div>
</section>

<section>
  <h2>3. When one core is loaded, placement matters</h2>
  <p>Core 1 spends 60% of its time on a competing load. With <b>static</b> placement (job i on core i mod {nw}, fixed up front) everyone waits for core 1's share. With <b>dynamic</b> placement the free cores take its jobs. Medians: static {imb['static']/1000:.2f} s, dynamic {imb['dynamic']/1000:.2f} s.</p>
  <div class="fig">{lanes(st)}<p class="src">static placement</p></div>
  <div class="fig">{lanes(dy)}<p class="src">dynamic placement</p></div>
</section>

<section>
  <h2>4. Other kinds of job, 1 core vs {nw}</h2>
  <p>Matmul jobs are tiny (a few ms). Over the cluster's serial mesh they scaled poorly; here the jobs move through shared memory, so they scale too.</p>
  <div class="fig"><table><tr><th>kind</th><th>1 core</th><th>{nw} cores</th><th>speedup</th></tr>{kind_rows}</table></div>
</section>

<section>
  <h2>Limits</h2>
  <ul>
    <li>Core 0 does not run jobs; the other cores run only the bounded compute kinds. Agents, the shell, devices and model calls stay on core 0.</li>
    <li>Jobs run to completion on a core (no preemption); a run can be cancelled between slices.</li>
    <li>The "competing load" is simulated by the kernel (a spin share per slice), not another real program.</li>
    <li>Virtual CPUs share {env.get('cpu', 'the host')}'s cores with Windows; scaling on real hardware is not measured.</li>
  </ul>
</section>
</main></body></html>"""


def main():
    src = sys.argv[1]
    out = sys.argv[2] if len(sys.argv) > 2 else os.path.join(os.path.dirname(__file__), "..", "..", "research-docs", "smp-report.html")
    open(out, "w", encoding="utf-8").write(build(src))
    print("wrote", os.path.abspath(out))


if __name__ == "__main__":
    main()
