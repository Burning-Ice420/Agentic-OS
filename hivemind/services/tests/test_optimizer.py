import os
import sys
import unittest

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..")))
from services.optimizer.search import bayes_opt, grid_search, random_search  # noqa: E402
from services.optimizer.space import Param, Space  # noqa: E402


def quad_space():
    s = Space("toy", [Param("x", list(range(10))), Param("y", list(range(10))), Param("mode", ["a", "b"])])
    s.constraints.append(lambda c: "x+y over ceiling" if c["x"] + c["y"] > 15 else None)
    return s


def f(c):
    return (c["x"] - 3) ** 2 + (c["y"] - 6) ** 2 + (0 if c["mode"] == "b" else 5)


class TestOptimizer(unittest.TestCase):
    def test_validator_blocks_security_and_out_of_range(self):
        s = quad_space()
        self.assertIn("security", s.validate({"x": 1, "y": 1, "mode": "a", "cap_enforce": False}))
        self.assertIn("outside", s.validate({"x": 99, "y": 1, "mode": "a"}))
        self.assertIn("ceiling", s.validate({"x": 9, "y": 9, "mode": "a"}))
        self.assertIsNone(s.validate({"x": 3, "y": 6, "mode": "b"}))

    def test_grid_finds_optimum_and_counts_invalid(self):
        tr = grid_search(quad_space(), f, budget=1000)
        self.assertEqual(tr.best()[1], 0.0)
        self.assertGreater(len(tr.invalid), 0)
        self.assertTrue(all(c["x"] + c["y"] <= 15 for c, _, _ in tr.evals), "never evaluates invalid configs")

    def test_grid_refuses_when_infeasible(self):
        self.assertIsNone(grid_search(quad_space(), f, budget=50))

    def test_budget_respected_and_curve_monotone(self):
        for tr in (random_search(quad_space(), f, 20, seed=1), bayes_opt(quad_space(), f, 20, seed=1)):
            self.assertLessEqual(len(tr.evals), 20)
            c = tr.curve()
            self.assertTrue(all(a >= b for a, b in zip(c, c[1:])))

    def test_bayes_beats_random_on_smooth_toy_over_seeds(self):
        b = [bayes_opt(quad_space(), f, 15, seed=s).best()[1] for s in range(8)]
        r = [random_search(quad_space(), f, 15, seed=s).best()[1] for s in range(8)]
        self.assertLessEqual(sum(b) / len(b), sum(r) / len(r))


if __name__ == "__main__":
    unittest.main()
