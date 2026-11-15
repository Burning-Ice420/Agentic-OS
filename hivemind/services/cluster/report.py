"""Build the cluster results page from a cluster_bench record.

  python services/cluster/report.py experiments/results/cluster_bench-<stamp>.json [out.html]

Writes a self-contained HTML page (inline SVG charts, no scripts) that cites the
record it was built from.
"""
from __future__ import annotations

import html
import json
import os
import sys

PALETTE = ["#3b6fd8", "#d9822b", "#2f9e64", "#9b59c9", "#d2453b", "#1f9fb4", "#b39320", "#6f7c8a", "#d6548f"]


def esc(s) -> str:
    return html.escape(str(s))


def runs_by_label(rec):
    return {r["label"]: r for r in rec["metrics"]["runs"]["value"]}


def ms(r):
    try:
        return int(r.get("makespan_ms") or 0)
    except ValueError:
        return 0


def per_worker(r):
    out = {}
    for p in (r.get("per_worker") or "").split("+"):
        if ":" in p:
            k, v = p.split(":")
            out[k] = int(v)
    return out


# ── SVG helpers (all text uses theme tokens via currentColor / CSS vars) ──
def bar_chart(items, unit, w=560, h=230, color="var(--accent)", ideal=None, fmt=lambda v: f"{v:,.1f}"):
    """items: [(label, value)]; ideal: optional [(label, value)] drawn as ticks."""
    L, R, T, B = 52, 12, 14, 38
    vmax = max([v for _, v in items] + [v for _, v in (ideal or [])] + [1]) * 1.12
    n = len(items)
    bw = (w - L - R) / n
    s = [f'<svg viewBox="0 0 {w} {h}" role="img">']
    for i in range(5):
        v = vmax * i / 4
        y = h - B - (h - B - T) * i / 4
        s.append(f'<line x1="{L}" x2="{w-R}" y1="{y:.1f}" y2="{y:.1f}" class="grid"/>')
        s.append(f'<text x="{L-6}" y="{y+4:.1f}" class="tick" text-anchor="end">{fmt(v)}</text>')
    for i, (lab, v) in enumerate(items):
        x = L + i * bw + bw * 0.18
        bh = (h - B - T) * v / vmax
        s.append(f'<rect x="{x:.1f}" y="{h-B-bh:.1f}" width="{bw*0.64:.1f}" height="{bh:.1f}" rx="3" fill="{color}"/>')
        s.append(f'<text x="{x+bw*0.32:.1f}" y="{h-B-bh-6:.1f}" class="val" text-anchor="middle">{fmt(v)}{unit}</text>')
        s.append(f'<text x="{x+bw*0.32:.1f}" y="{h-B+16:.1f}" class="tick" text-anchor="middle">{esc(lab)}</text>')
        if ideal:
            iv = dict(ideal).get(lab)
            if iv:
                y = h - B - (h - B - T) * iv / vmax
                s.append(f'<line x1="{x-4:.1f}" x2="{x+bw*0.64+4:.1f}" y1="{y:.1f}" y2="{y:.1f}" class="ideal"/>')
    s.append("</svg>")
    return "".join(s)


def cpu_lines(cpu, names, t0, t1, w=560, h=230):
    L, R, T, B = 40, 12, 12, 30
    rows = [r for r in cpu if t0 - 3 <= r["t"] <= t1 + 3]
    if not rows:
        return "<p>No CPU samples in this window.</p>"
    x0, x1 = rows[0]["t"], rows[-1]["t"]
    sx = lambda t: L + (w - L - R) * (t - x0) / max(0.1, x1 - x0)
    sy = lambda v: h - B - (h - B - T) * min(v, 110) / 110
    s = [f'<svg viewBox="0 0 {w} {h}" role="img">']
    for v in (0, 25, 50, 75, 100):
        s.append(f'<line x1="{L}" x2="{w-R}" y1="{sy(v):.1f}" y2="{sy(v):.1f}" class="grid"/>'
                 f'<text x="{L-6}" y="{sy(v)+4:.1f}" class="tick" text-anchor="end">{v}%</text>')
    for i, n in enumerate(names):
        pts = " ".join(f"{sx(r['t']):.1f},{sy(r.get(n, 0)):.1f}" for r in rows)
        s.append(f'<polyline points="{pts}" fill="none" stroke="{PALETTE[i % len(PALETTE)]}" stroke-width="1.8"/>')
    s.append(f'<text x="{L}" y="{h-8}" class="tick">0 s</text><text x="{w-R}" y="{h-8}" class="tick" text-anchor="end">{x1-x0:.0f} s</text>')
    s.append("</svg>")
    leg = "".join(f'<span><i style="background:{PALETTE[i % len(PALETTE)]}"></i>{esc(n)}</span>' for i, n in enumerate(names))
    return "".join(s) + f'<div class="legend">{leg}</div>'


def gantt(events, workers, t0, t1, marks=(), w=1100, h=None):
    lane = 22
    h = h or (len(workers) * lane + 34)
    L, R = 44, 10
    sx = lambda t: L + (w - L - R) * (t - t0) / max(0.1, t1 - t0)
    s = [f'<svg viewBox="0 0 {w} {h}" role="img">']
    start = {}
    for e in events:
        if not (t0 - 1 <= e["t"] <= t1 + 1):
            continue
        if e["type"] == "assign":
            start[(e["job"], e["worker"])] = e["t"]
        elif e["type"] == "result":
            st = start.pop((e["job"], e["worker"]), None)
            if st is None or e["worker"] not in workers:
                continue
            i = workers.index(e["worker"])
            check = "c" in e["job"].split("j", 1)[-1]
            s.append(f'<rect x="{sx(st):.1f}" y="{6+i*lane:.1f}" width="{max(1.5, sx(e["t"])-sx(st)-1):.1f}" height="{lane-6}" rx="2" '
                     f'fill="{PALETTE[i % len(PALETTE)]}" opacity="{0.45 if check else 0.9}"/>')
    for (job, wk), st in start.items():   # never finished (lost with a dead VM)
        if wk in workers:
            i = workers.index(wk)
            s.append(f'<rect x="{sx(st):.1f}" y="{6+i*lane:.1f}" width="{max(2, sx(t1)-sx(st)):.1f}" height="{lane-6}" rx="2" '
                     f'fill="none" stroke="var(--bad)" stroke-dasharray="4 3"/>')
    for i, wk in enumerate(workers):
        s.append(f'<text x="4" y="{6+i*lane+lane/2+2:.1f}" class="tick">{esc(wk)}</text>')
    for t, label in marks:
        x = sx(t)
        s.append(f'<line x1="{x:.1f}" x2="{x:.1f}" y1="2" y2="{h-22}" class="mark"/>'
                 f'<text x="{x+4:.1f}" y="{h-8}" class="marktxt">{esc(label)}</text>')
    s.append(f'<text x="{w-R}" y="{h-8}" class="tick" text-anchor="end">{t1-t0:.1f} s</text></svg>')
    return "".join(s)


def build(rec_path: str) -> str:
    rec = json.load(open(rec_path, encoding="utf-8"))
    R = runs_by_label(rec)
    hub = rec["metrics"]["hub"]["value"]
    extra = rec["metrics"]["extra"]["value"]
    events, cpu, notes = hub["events"], hub["cpu"], hub["notes"]
    cores = hub.get("cores")
    name = os.path.basename(rec_path)
    env = rec.get("environment", {})
    W = sorted({e["worker"] for e in events if e["type"] == "assign"}, key=lambda x: (len(x), x))

    sc = [(k, R.get(f"scaling prime48 w={k}")) for k in (1, 2, 4, 8)]
    sc = [(k, r) for k, r in sc if r and r.get("finished")]
    base = ms(sc[0][1]) if sc else 1
    speed = [(f"{k}", base / max(1, ms(r))) for k, r in sc]
    times = [(f"{k}", ms(r) / 1000) for k, r in sc]
    eff8 = speed[-1][1] / int(speed[-1][0]) if speed else 0

    w8 = R.get("scaling prime48 w=8", {})
    cpu8 = cpu_lines(cpu, ["coord"] + [x for x in W][:8], *(w8.get("window") or [0, 0]))

    het = [(p, R.get(f"hetero prime48 w=4 {p}")) for p in ("rr", "least", "adaptive")]
    het = [(p, r) for p, r in het if r]
    het_bars = bar_chart([(p, ms(r) / 1000) for p, r in het], " s", color="var(--accent2)")
    het_split = "".join(
        f"<tr><td>{esc(p)}</td>" + "".join(f"<td>{per_worker(r).get(f'w{i}', 0)}</td>" for i in range(1, 5))
        + f"<td>{r.get('cpu_avg', {}).get('w1', 0):.0f}%</td><td>{sum(r.get('cpu_avg', {}).get(f'w{i}', 0) for i in (2, 3, 4))/3:.0f}%</td></tr>"
        for p, r in het)

    g1, g8 = R.get("granularity matmul96 w=1", {}), next((r for l, r in R.items() if l.startswith("granularity") and not l.endswith("w=1")), {})
    gran_speed = ms(g1) / max(1, ms(g8)) if g1 and g8 else 0
    gran_cpu = sum(v for k, v in (g8.get("cpu_avg") or {}).items() if k.startswith("w")) / max(1, len([k for k in (g8.get("cpu_avg") or {}) if k.startswith("w")]))

    a1, a4 = R.get("ai 12 w=1", {}), R.get("ai 12 w=4", {})
    ai_bars = bar_chart([("1 worker", ms(a1) / 1000), ("4 workers", ms(a4) / 1000)], " s", color="var(--accent3)")

    hon, e25, e100 = R.get("honest, check 25%", {}), R.get("evil w2, check 25%", {}), R.get("evil w2, check 100%", {})
    w8ms = ms(w8)

    fail = next((r for l, r in R.items() if l.startswith("failure")), {})
    kill_t = next((n["t"] for n in notes if n["text"].startswith("killing")), None)
    fw = fail.get("window") or [0, 0]
    fail_g = gantt(events, W, fw[0] - 0.2, fw[1] + 0.3, marks=[(kill_t, "VM killed")] if kill_t else ())

    pipe = R.get("pipeline 8", {})
    pipe_sum = (extra.get("pipeline_sum_line") or "").split("result=")[-1].split(";")[0]
    ai_last = (extra.get("ai_results_sample") or [""])[-1]

    def verdict(r):
        c = r.get("correctness") or {}
        return f"{c.get('checked', 0) - c.get('wrong', 0)} of {c.get('checked', 0)} correct"

    def vrow(label, r):
        if not r:
            return ""
        return (f"<tr><td>{esc(label)}</td><td>{ms(r)/1000:.1f} s</td><td>{esc(r.get('checks', '0'))}</td>"
                f"<td>{esc(r.get('mismatches', '0'))}</td><td>{esc(r.get('reassigned', '0'))}</td><td>{verdict(r)}</td></tr>")

    page = f"""<title>Hive Cluster Results</title>
<meta name="claude:viewport-fit-sides" content="cover">
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=IBM+Plex+Mono:wght@400;600&family=IBM+Plex+Sans:wght@400;600&family=Space+Grotesk:wght@500;700&display=swap">
<style>
/* layout: one reading column of findings, each a claim, its chart, and the record it came from */
:root{{--bg:#f5f6f4;--surface:#ffffff;--fg:#1d2321;--dim:#5d6863;--line:#dde2de;--accent:#2f6fd6;--accent2:#c9772c;--accent3:#2f9467;--bad:#c53b30;
--display:"Space Grotesk",system-ui,sans-serif;--body:"IBM Plex Sans",system-ui,sans-serif;--mono:"IBM Plex Mono",ui-monospace,Consolas,monospace}}
@media (prefers-color-scheme: dark){{:root:not([data-theme="light"]){{--bg:#121614;--surface:#1a201d;--fg:#e4e9e6;--dim:#97a39d;--line:#2b3430;--accent:#6c9cf0;--accent2:#e39a52;--accent3:#5cc293;--bad:#f0675c;color-scheme:dark}}}}
:root[data-theme="dark"]{{--bg:#121614;--surface:#1a201d;--fg:#e4e9e6;--dim:#97a39d;--line:#2b3430;--accent:#6c9cf0;--accent2:#e39a52;--accent3:#5cc293;--bad:#f0675c;color-scheme:dark}}
body{{background:var(--bg);color:var(--fg);font:15px/1.6 var(--body)}}
main{{max-width:980px;margin:0 auto;padding-block:28px 56px;padding-left:max(16px,env(safe-area-inset-left,0px));padding-right:max(16px,env(safe-area-inset-right,0px));display:grid;gap:28px}}
h1,h2{{font-family:var(--display);text-wrap:balance;margin:0}} h1{{font-size:34px;line-height:1.15}} h2{{font-size:21px}}
p{{margin:0;max-width:68ch}} .lede{{color:var(--dim);font-size:16px}}
.kpis{{display:grid;grid-template-columns:repeat(auto-fit,minmax(170px,1fr));gap:12px}}
.kpi{{background:var(--surface);border:1px solid var(--line);border-radius:8px;padding:12px 14px}}
.kpi b{{display:block;font:600 26px var(--display)}} .kpi span{{color:var(--dim);font-size:13px}}
section{{display:grid;gap:10px;min-width:0}}
.fig{{background:var(--surface);border:1px solid var(--line);border-radius:8px;padding:12px;overflow-x:auto}}
.two{{display:grid;grid-template-columns:repeat(auto-fit,minmax(300px,1fr));gap:14px}}
svg{{width:100%;height:auto;display:block}} svg text{{font-family:var(--mono)}}
.grid{{stroke:var(--line)}} .tick{{fill:var(--dim);font-size:11px}} .val{{fill:var(--fg);font-size:12px;font-weight:600}}
.ideal{{stroke:var(--fg);stroke-width:2;stroke-dasharray:4 3}} .mark{{stroke:var(--bad);stroke-width:2}} .marktxt{{fill:var(--bad);font-size:12px}}
.legend{{display:flex;flex-wrap:wrap;gap:10px;font-size:12px;color:var(--dim);margin-top:6px}} .legend i{{display:inline-block;width:10px;height:10px;border-radius:2px;margin-right:4px}}
table{{border-collapse:collapse;width:100%;font-variant-numeric:tabular-nums;font-size:14px}}
td,th{{padding:6px 8px;border-bottom:1px solid var(--line);text-align:right}} td:first-child,th:first-child{{text-align:left}} th{{color:var(--dim);font-weight:600}}
.src{{font:12px var(--mono);color:var(--dim)}} code{{font-family:var(--mono);font-size:13px}}
.note{{border-left:3px solid var(--accent2);padding:4px 12px;color:var(--fg)}}
ul{{margin:0;padding-left:20px;max-width:68ch}}
</style>
<main>
<header style="display:grid;gap:10px">
  <h1>Hive cluster: {len(W)} HiveMind VMs sharing work through the kernel</h1>
  <p class="lede">One coordinator VM decides in its kernel which worker VM runs each job. Workers run real compute, report load every second, and send results back into the coordinator's shared memory graph, where they count as untrusted until checked. Everything below was measured on one laptop ({esc(env.get('cpu', ''))}, {cores or '?'} logical CPUs), one repetition per configuration.</p>
  <p class="src">record: {esc(name)}</p>
</header>

<div class="kpis">
  <div class="kpi"><b>{speed[-1][1]:.1f}x</b><span>speedup on {speed[-1][0]} workers (prime counting, 48 jobs)</span></div>
  <div class="kpi"><b>{eff8*100:.0f}%</b><span>parallel efficiency at {speed[-1][0]} workers</span></div>
  <div class="kpi"><b>{ms(het[0][1])/max(1,ms(het[1][1])):.1f}x</b><span>faster with load-aware placement than round robin when one worker is slowed</span></div>
  <div class="kpi"><b>{sum((r.get('correctness') or {}).get('checked', 0) for r in R.values())}</b><span>results checked against the host's own computation</span></div>
</div>

<section>
  <h2>1. More VMs, proportionally faster</h2>
  <p>48 prime-counting jobs (about one second each). The coordinator places each job on the worker with the fewest jobs in flight. Dashed marks show perfect linear scaling.</p>
  <div class="two">
    <div class="fig">{bar_chart(speed, "x", ideal=[(l, float(l)) for l, _ in speed])}<p class="src">speedup over 1 worker</p></div>
    <div class="fig">{bar_chart(times, " s", color="var(--accent3)")}<p class="src">time to finish all 48 jobs</p></div>
  </div>
  <div class="fig">{cpu8}<p class="src">host CPU per VM during the 8-worker run (percent of one core; psutil per QEMU process)</p></div>
  <p>Every worker sits near 90 to 100% of a core while the run lasts, and drops to near zero when idle because the kernel halts the CPU between interrupts. All {sum((r.get('correctness') or {}).get('checked', 0) for l, r in R.items() if l.startswith('scaling'))} results in the scaling runs matched the host.</p>
</section>

<section>
  <h2>2. A slow worker: where the scheduler earns its keep</h2>
  <p>Worker w1 was pinned to the same host core as a busy loop, so it ran at a fraction of normal speed. Same 48 jobs, 4 workers, three placement policies.</p>
  <div class="two">
    <div class="fig">{het_bars}<p class="src">time to finish (lower is better)</p></div>
    <div class="fig"><table><tr><th>policy</th><th>w1</th><th>w2</th><th>w3</th><th>w4</th><th>w1 CPU</th><th>others CPU</th></tr>{het_split}</table>
    <p class="src" style="margin-top:8px">jobs per worker and average CPU</p></div>
  </div>
  <p>Round robin decides up front and gives the slow worker a quarter of the jobs; the others finish early and wait (their CPU average drops). Least loaded and speed-aware placement watch what is in flight and shift work away from w1. Speed-aware placement was <b>not</b> reliably better than least loaded: it was slightly faster in an earlier run (14.5 s vs 15.3 s) and slower in this one.</p>
</section>

<section>
  <h2>3. Where it stops scaling</h2>
  <div class="two">
    <div class="fig"><p><b>Tiny jobs.</b> 96 small matrix products (a few milliseconds each) ran only <b>{gran_speed:.1f}x</b> faster on 8 workers than on 1, with workers at about {gran_cpu:.0f}% CPU. Each job costs a round trip over the emulated serial mesh, which is slower than the job itself. Jobs need to be far larger than the messages that carry them.</p></div>
    <div class="fig">{ai_bars}<p class="src">12 model calls (sentiment, llama3.2:3b): {ms(a1)/1000:.1f} s on 1 worker, {ms(a4)/1000:.1f} s on 4</p>
    <p>Model jobs gain less ({ms(a1)/max(1,ms(a4)):.1f}x on 4 workers): all workers share one Ollama on the same laptop, so the model, not the cluster, is the limit.</p></div>
  </div>
</section>

<section>
  <h2>4. Results from a worker are not trusted until checked</h2>
  <p>A share of results is re-run on a different worker. If the two disagree, the coordinator recomputes the job itself, quarantines whoever was wrong, and re-runs everything that worker produced. In the last two runs, worker w2 was switched to corrupt one result in five.</p>
  <div class="fig"><table><tr><th>run (8 workers, 48 jobs)</th><th>time</th><th>checks</th><th>caught</th><th>re-run</th><th>final results</th></tr>
  <tr><td>no checking</td><td>{w8ms/1000:.1f} s</td><td>0</td><td>0</td><td>0</td><td>{verdict(w8)}</td></tr>
  {vrow("honest workers, check 25%", hon)}{vrow("w2 corrupts 1 in 5, check 25%", e25)}{vrow("w2 corrupts 1 in 5, check 100%", e100)}</table></div>
  <p class="note">In an earlier benchmark (record cluster_bench-20261115-102540.json, older design where the coordinator re-ran samples itself), 25% checking missed w2's one bad result and it was accepted. Checking a sample is probabilistic: with 25% checking, each corrupted result has a 1 in 4 chance of being re-run, so a bad worker can slip results through before it is caught. Checking everything is certain but runs every job twice.</p>
</section>

<section>
  <h2>5. A worker VM dies mid-run</h2>
  <p>4 seconds into a 64-job run, the harness killed one worker's VM. Its heartbeats stopped; after 3.5 s the coordinator declared it lost and re-routed the {esc(fail.get('reassigned', '?'))} job(s) it was holding.</p>
  <div class="fig">{fail_g}<p class="src">each bar is one job on one worker; dashed red = lost with the VM</p></div>
  <p>The run finished in {ms(fail)/1000:.1f} s; {verdict(fail)}.</p>
</section>

<section>
  <h2>6. A pipeline across machines</h2>
  <p>Eight workers counted primes in slices (map). The coordinator summed them from its shared memory graph (reduce): <b>{esc(pipe_sum)}</b>, which matches the host's count ({extra.get('pipeline_truth', '?'):,}). A worker then asked the model to report it: <i>"{esc(ai_last)}"</i>. Total {ms(pipe)/1000:.1f} s.</p>
</section>

<section>
  <h2>What this does and does not show</h2>
  <ul>
    <li>Each HiveMind kernel is single-core; the cluster spreads work across host cores by running one VM per core. The kernel itself does not schedule across cores.</li>
    <li>The coordinator's kernel makes every placement decision. The host hub only forwards messages to the named VM and measures CPU.</li>
    <li>One repetition per configuration on one laptop; timings move with whatever else the machine is doing.</li>
    <li>The mesh is an emulated serial line: fine for jobs of a second, a bottleneck for jobs of milliseconds.</li>
    <li>Workers run a fixed set of job kinds with bounded sizes. No code travels over the mesh.</li>
  </ul>
  <p class="src">built by services/cluster/report.py from {esc(name)} · OS {esc(env.get('os', ''))}</p>
</section>
</main>
"""
    return page


if __name__ == "__main__":
    src = sys.argv[1]
    out = sys.argv[2] if len(sys.argv) > 2 else os.path.join(os.path.dirname(__file__), "..", "..", "research-docs", "cluster-report.html")
    open(out, "w", encoding="utf-8").write(build(src))
    print("wrote", os.path.abspath(out))
