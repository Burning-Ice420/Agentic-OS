import os
import sys
import unittest

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..")))
from services.scheduler.dag import Task, Workflow  # noqa: E402
from services.scheduler.metrics import summarize  # noqa: E402
from services.scheduler.sim import simulate  # noqa: E402
from services.scheduler.workload import generate  # noqa: E402


def wf(wid, release, deadline, spec, prio=0):
    w = Workflow(wid, release, deadline)
    for tid, deps, dur, res in spec:
        w.tasks[tid] = Task(tid, deps, dur, dur, res, prio)
    return w


class TestScheduler(unittest.TestCase):
    def test_dependencies_respected_and_slots_never_exceeded(self):
        ws = generate(seed=3, n=40, rate=1.0)
        r = simulate(ws, {"model": 2, "cpu": 2}, "dep_aware", fail_prob=0.1, seed=3)
        end = {(t.wid, t.tid): t.end for t in r.tasks}
        start = {}
        for s, e, res, wid, tid, ok in r.trace:
            start.setdefault((wid, tid), s)
        for w in ws:
            for t in w.tasks.values():
                if (w.wid, t.tid) in start:
                    for d in t.deps:
                        self.assertGreaterEqual(start[(w.wid, t.tid)], end[(w.wid, d)] - 1e-9)
        for res, cap in {"model": 2, "cpu": 2}.items():
            pts = sorted([(s, 1) for s, e, rr, *_ in r.trace if rr == res] +
                         [(e, -1) for s, e, rr, *_ in r.trace if rr == res], key=lambda x: (x[0], x[1]))
            cur = 0
            for _, d in pts:
                cur += d
                self.assertLessEqual(cur, cap)

    def test_deterministic_given_seed(self):
        a = summarize(simulate(generate(1, 30, 1.0), {"model": 1, "cpu": 2}, "edf", fail_prob=0.1, seed=7))
        b = summarize(simulate(generate(1, 30, 1.0), {"model": 1, "cpu": 2}, "edf", fail_prob=0.1, seed=7))
        self.assertEqual(a, b)

    def test_retries_are_bounded(self):
        ws = [wf("w", 0, 100, [("a", [], 1.0, "model")])]
        r = simulate(ws, {"model": 1}, "fifo", fail_prob=1.0, seed=0)
        self.assertEqual(r.tasks[0].attempts, 2)  # max_retries=1 -> 2 attempts
        self.assertFalse(r.workflows["w"]["ok"])

    def test_cycle_rejected(self):
        w = wf("w", 0, 10, [("a", ["b"], 1, "cpu"), ("b", ["a"], 1, "cpu")])
        with self.assertRaises(ValueError):
            simulate([w], {"cpu": 1}, "fifo")

    def test_dep_aware_beats_fifo_on_constructed_case(self):
        # One model slot busy until t=3. A loose task becomes ready at t=1, an urgent
        # one at t=2. FIFO serves the loose one first and the urgent one misses.
        def case():
            return [wf("blocker", 0.0, 100.0, [("z", [], 3, "model")]),
                    wf("loose", 1.0, 100.0, [("l", [], 3, "model")]),
                    wf("urgent", 2.0, 4.5, [("u", [], 1, "model")])]
        fifo = summarize(simulate(case(), {"model": 1}, "fifo"))
        dep = summarize(simulate(case(), {"model": 1}, "dep_aware"))
        edf = summarize(simulate(case(), {"model": 1}, "edf"))
        self.assertAlmostEqual(fifo["deadline_miss_rate"], 1 / 3)
        self.assertEqual(dep["deadline_miss_rate"], 0.0)
        self.assertEqual(edf["deadline_miss_rate"], 0.0)


if __name__ == "__main__":
    unittest.main()
