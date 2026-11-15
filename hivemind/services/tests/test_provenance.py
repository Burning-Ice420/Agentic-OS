"""Provenance service tests against a workflow whose lineage is known exactly.

Ground truth (instance A = "rtA", instance B = "rtB"):
  shell writes kernel-root/temp (A:3)
  agent 2 reads temp (A:4) -> fires (A:5) -> writes alert (A:6) -> mesh_send (A:7)
  B receives: peer write sensors... shared/alert (B:2), no explicit parent -> inferred A:7
  agent 2 offloads: ctx read (A:8) <- fire (A:5); offload (A:9) <- A:8; model write (A:10) <- A:9
  policy: enforce on (A:11), deny agent 3 write (A:12), allow agent 2 write (A:13) used by write A:14
"""
import os
import sys
import unittest

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..")))
from services.provenance import Lineage, parse_line, ParseError  # noqa: E402
from services.provenance.graph import format_chain  # noqa: E402

A = [
    "PEV|1|rtA|1|1|create|kernel|1|kernel-root|-|-|",
    "PEV|1|rtA|2|1|write|kernel|1|kernel-root|version|-|len=3;vh=aaaaaaaa",
    "PEV|1|rtA|3|10|write|shell|1|kernel-root|temp|-|len=2;vh=bbbbbbbb",
    "PEV|1|rtA|4|18|read|agent:2|1|kernel-root|temp|-|len=2;vh=bbbbbbbb;rule=0;cond=>80",
    "PEV|1|rtA|5|18|agent_fire|agent:2|1|kernel-root|alert|4|rule=0",
    "PEV|1|rtA|6|18|write|agent:2|1|kernel-root|alert|5|len=4;vh=cccccccc",
    "PEV|1|rtA|7|18|mesh_send|agent:2|1|kernel-root|alert|6|len=4;vh=cccccccc",
    "PEV|1|rtA|8|36|read|agent:2|1|kernel-root|*|5|len=0;purpose=offload_context",
    "PEV|1|rtA|9|36|offload|agent:2|1|kernel-root|*|8|tool=ai;prompt_len=11;ctx_len=30",
    "PEV|1|rtA|10|90|write|model:agent:2|1|kernel-root|decision|9|len=4;vh=dddddddd",
    "PEV|1|rtA|11|95|cap_change|shell|0|-|enforce_on|-|",
    "PEV|1|rtA|12|96|cap_deny|agent:3|1|kernel-root|alert|-|op=w;reason=no_capability;pv=3",
    "PEV|1|rtA|13|97|cap_allow|agent:2|1|kernel-root|alert|5|op=w;cap=1;pv=3",
    "PEV|1|rtA|14|97|write|agent:2|1|kernel-root|alert|5|len=4;vh=cccccccc;auth=13",
    "PEV|1|rtA|15|98|marker|shell|0|-|demo|-|",
    "PEV|1|rtA|16|99|write|shell|1|kernel-root|x|-|len=1;vh=eeeeeeee",
]
B = [
    "PEV|1|rtB|1|5|create|peer|7|kernel-root|-|-|",
    "PEV|1|rtB|2|5|write|peer|7|kernel-root|alert|-|len=4;vh=cccccccc",
    "PEV|1|rtB|3|6|write|peer|7|kernel-root|orphan|-|len=1;vh=ffffffff",
]


class TestParse(unittest.TestCase):
    def test_valid_line(self):
        kind, ev = parse_line(A[5])
        self.assertEqual(kind, "event")
        self.assertEqual((ev.id, ev.kind, ev.actor, ev.parent), (6, "write", "agent:2", 5))
        self.assertEqual(ev.to_envelope()["parent_event_ids"], ["rtA:5"])
        self.assertEqual(ev.to_envelope()["agent_id"], "agent:2")

    def test_malformed_lines_are_rejected_not_guessed(self):
        for bad in ["PEV|1|rt|x|1|write|shell|1|m|k|-|",          # non-int id
                    "PEV|2|rt|1|1|write|shell|1|m|k|-|",          # future schema
                    "PEV|1|rt|1|1|teleport|shell|1|m|k|-|",       # unknown kind
                    "PEV|1|rt|1|1|write|shell|1|m|k|-",           # missing field
                    "PEV|1|rt|1|1|write|shell|1|m|k|-|novalue"]:  # bad attrs
            with self.assertRaises((ParseError, ValueError)):
                parse_line(bad)

    def test_non_event_lines_ignored(self):
        self.assertIsNone(parse_line("[boot] heap initialized"))


class TestLineage(unittest.TestCase):
    def setUp(self):
        self.g = Lineage.from_lines(A + B + ["garbage PEV|broken", "PEV|1|rtA|zz|bad"])

    def test_agent_write_chain_reaches_the_read_that_triggered_it(self):
        steps = self.g.chain(self.g.latest_write("kernel-root", "alert", "rtA"))
        gids = [s["gid"] for s in steps]
        self.assertEqual(gids, ["rtA:14", "rtA:5", "rtA:4", "rtA:3"])
        self.assertEqual(steps[-1]["link"], "read_from", "the read is linked to the write it observed")
        self.assertEqual(steps[-1]["origin"], "root", "that write came from the trusted shell")

    def test_model_write_links_through_offload_to_the_firing(self):
        steps = self.g.chain("rtA:10")
        self.assertEqual([s["gid"] for s in steps], ["rtA:10", "rtA:9", "rtA:8", "rtA:5", "rtA:4", "rtA:3"])
        self.assertTrue(all(s["link"] in ("start", "explicit") for s in steps[:-1]))
        self.assertEqual(steps[-1]["link"], "read_from")

    def test_cross_instance_link_is_inferred_and_labelled(self):
        steps = self.g.chain("rtB:2")
        self.assertEqual(steps[1]["gid"], "rtA:7")
        self.assertEqual(steps[1]["link"], "inferred")
        self.assertEqual([s["gid"] for s in steps], ["rtB:2", "rtA:7", "rtA:6", "rtA:5", "rtA:4", "rtA:3"])

    def test_unmatched_peer_write_is_unknown_not_fabricated(self):
        steps = self.g.chain("rtB:3")
        self.assertEqual(len(steps), 1)
        self.assertEqual(steps[0]["origin"], "unknown")

    def test_ambiguous_inference_is_refused(self):
        dup = A + ["PEV|1|rtC|1|1|mesh_send|shell|1|kernel-root|alert|-|len=4;vh=cccccccc"] + B
        g = Lineage.from_lines(dup)
        self.assertNotIn("rtB:2", g.inferred)
        self.assertEqual(g.ambiguous["rtB:2"], 2)
        self.assertEqual(g.chain("rtB:2")[0]["ambiguous_candidates"], 2)

    def test_shell_write_is_a_trusted_root(self):
        steps = self.g.chain("rtA:3")
        self.assertEqual(steps[0]["origin"], "root")

    def test_dropped_parent_is_reported_missing(self):
        g = Lineage.from_lines(["PEV|1|rtD|9|1|write|agent:1|1|m|k|7|len=1;vh=00000000"])
        steps = g.chain("rtD:9")
        self.assertTrue(steps[-1].get("missing"))
        self.assertEqual(g.stats()["dangling_parents"], 1)

    def test_id_gaps_count_drops(self):
        g = Lineage.from_lines(["PEV|1|r|1|1|marker|shell|0|-|a|-|", "PEV|1|r|5|1|marker|shell|0|-|b|-|"])
        self.assertEqual(g.id_gaps()["r"], 3)

    def test_policy_explain(self):
        d = self.g.explain("rtA:12")
        self.assertEqual((d["decision"], d["requester"], d["operation"], d["deny_reason"], d["policy_version"]),
                         ("DENY", "agent:3", "w", "no_capability", "3"))
        self.assertIn("enforce_on@11", d["policy_changes_before"])
        a = self.g.explain("rtA:13")
        self.assertEqual((a["decision"], a["matched_capability"], a["authorized_events"]), ("ALLOW", "1", ["rtA:14"]))

    def test_workflow_slice(self):
        evs = self.g.workflow("demo")
        self.assertEqual([e.id for e in evs], [15, 16])

    def test_stats_and_malformed_count(self):
        s = self.g.stats()
        self.assertEqual(s["malformed_lines"], 1)  # 'garbage' line is not PEV-prefixed so ignored; one bad PEV
        self.assertEqual(s["writes"], s["writes_with_actor"])
        self.assertEqual(s["agent_or_model_writes"], s["agent_or_model_writes_with_parent"])
        self.assertEqual((s["peer_writes_inferred"], s["peer_writes_unknown"]), (1, 1))

    def test_format_chain_renders(self):
        txt = format_chain(self.g, self.g.chain("rtB:2"))
        self.assertIn("[inferred]", txt)


if __name__ == "__main__":
    unittest.main()


class TestRealTraceFixture(unittest.TestCase):
    """Regression test on a trace recorded from a real guest (experiments/phase12_live.py)."""

    def setUp(self):
        p = os.path.join(os.path.dirname(__file__), "fixtures", "live_phase12.log")
        if not os.path.exists(p):
            self.skipTest("fixture not recorded yet")
        self.g = Lineage.from_files([p])

    def test_real_trace_reconstructs(self):
        g = self.g
        w = [e for e in g.events.values() if e.kind == "write" and e.key == "alert" and e.actor == "agent:2"]
        self.assertTrue(w)
        kinds = [s["event"].kind for s in g.chain(min(w, key=lambda e: e.id).gid)]
        self.assertEqual(kinds, ["write", "agent_fire", "read", "write"])
        s = g.stats()
        self.assertEqual(s["malformed_lines"], 0)
        self.assertTrue(all(v == 0 for v in s["id_gaps"].values()))
        self.assertFalse(any(e.kind == "write" and e.actor == "agent:3" for e in g.events.values()))


class TestReadFromAndTaint(unittest.TestCase):
    # peer writes doc (2); agent 1 reads doc (3) -> fire (4) -> writes notes (5);
    # agent 2 reads notes (6) -> fire (7) -> context read * (8) -> offload (9)
    L = [
        "PEV|1|r|1|1|create|peer|1|work|-|-|",
        "PEV|1|r|2|1|write|peer|1|work|doc|-|len=5;vh=11111111",
        "PEV|1|r|3|2|read|agent:1|1|work|doc|-|len=5;vh=11111111",
        "PEV|1|r|4|2|agent_fire|agent:1|1|work|notes|3|rule=0",
        "PEV|1|r|5|2|write|agent:1|1|work|notes|4|len=7;vh=22222222",
        "PEV|1|r|6|3|read|agent:2|1|work|notes|-|len=7;vh=22222222",
        "PEV|1|r|7|3|agent_fire|agent:2|1|work|-|6|rule=0",
        "PEV|1|r|8|3|read|agent:2|1|work|*|7|len=0;purpose=offload_context",
        "PEV|1|r|9|3|offload|agent:2|1|work|*|8|tool=ai",
    ]

    def test_read_links_to_the_write_it_observed(self):
        g = Lineage.from_lines(self.L)
        self.assertEqual(g.read_from["r:3"], ["r:2"])
        self.assertEqual(g.read_from["r:6"], ["r:5"])
        self.assertEqual(sorted(g.read_from["r:8"]), ["r:2", "r:5"], "whole-node read sees every key")

    def test_untrusted_peer_content_reaches_offload(self):
        t = Lineage.from_lines(self.L).taint("r:9")
        self.assertTrue(t["tainted"])
        self.assertEqual(t["sources"][0]["untrusted_write"], "r:2")
        self.assertEqual(t["sources"][0]["path"][-1], "r:9")

    def test_trusted_control_is_not_tainted(self):
        L = [ln.replace("|write|peer|", "|write|shell|").replace("|create|peer|", "|create|shell|") for ln in self.L]
        self.assertFalse(Lineage.from_lines(L).taint("r:9")["tainted"])

    def test_hash_mismatch_is_flagged_not_linked(self):
        L = self.L[:2] + ["PEV|1|r|3|2|read|agent:1|1|work|doc|-|len=5;vh=99999999"]
        g = Lineage.from_lines(L)
        self.assertNotIn("r:3", g.read_from)
        self.assertEqual(g.read_inconsistent, ["r:3"])


class TestRealInjectionFixture(unittest.TestCase):
    """Real guest run of experiments/mvp_injection.py (peer-injected doc, 3 agents, model tool)."""

    def setUp(self):
        p = os.path.join(os.path.dirname(__file__), "fixtures", "live_mvp_injection.log")
        if not os.path.exists(p):
            self.skipTest("fixture not recorded yet")
        self.g = Lineage.from_files([p])

    def _offload(self, node):
        return next(e for e in self.g.events.values() if e.kind == "offload" and e.mem_name == node)

    def test_untrusted_doc_reaches_tool_call(self):
        t = self.g.taint(self._offload("inbox").gid)
        self.assertTrue(t["tainted"])
        self.assertIn("inbox/doc", [s["target"] for s in t["sources"]])

    def test_trusted_control_not_flagged(self):
        self.assertFalse(self.g.taint(self._offload("local").gid)["tainted"])

    def test_model_answer_denied_for_read_only_reporter(self):
        denies = [e for e in self.g.events.values() if e.kind == "cap_deny" and e.actor.startswith("model:")]
        self.assertEqual(len(denies), 2)
        self.assertTrue(all(e.parent_gid and self.g.events[e.parent_gid].kind == "offload" for e in denies))


class TestRealTwoVmFixture(unittest.TestCase):
    """Real two-guest mesh run of experiments/two_vm_mesh.py."""

    def setUp(self):
        d = os.path.join(os.path.dirname(__file__), "fixtures")
        ps = [os.path.join(d, f"live_mesh_{t}.log") for t in ("A", "B")]
        if not all(os.path.exists(p) for p in ps):
            self.skipTest("fixtures not recorded yet")
        self.g = Lineage.from_files(ps)

    def test_chain_crosses_machines_to_the_shell_write(self):
        g = self.g
        act = next(e for e in g.events.values() if e.kind == "write" and e.key == "action" and e.actor.startswith("agent"))
        steps = g.chain(act.gid)
        self.assertIn("inferred", [s["link"] for s in steps])
        last = steps[-1]["event"]
        self.assertEqual((last.kind, last.actor, last.key), ("write", "shell", "temp"))
        self.assertNotEqual(last.runtime, act.runtime)

    def test_no_ambiguous_or_unknown_peer_writes(self):
        s = self.g.stats()
        self.assertEqual(s["peer_writes_ambiguous"], 0)
        self.assertEqual(len(s["instances"]), 2)


class TestSentKeys(unittest.TestCase):
    def test_whole_node_read_links_only_the_keys_sent(self):
        L = [
            "PEV|1|r|1|1|write|shell|1|notes|n1|-|len=3;vh=1",
            "PEV|1|r|2|1|write|peer|1|notes|n2|-|len=3;vh=2;taint=1",
            "PEV|1|r|3|1|write|shell|1|notes|question|-|len=3;vh=3",
            "PEV|1|r|4|2|read|shell|1|notes|*|-|len=0;purpose=offload_context;sent=n1;redacted=1",
            "PEV|1|r|5|2|offload|shell|1|notes|*|4|tool=ai",
        ]
        g = Lineage.from_lines(L)
        self.assertEqual(g.read_from["r:4"], ["r:1"])
        self.assertFalse(g.taint("r:5")["tainted"], "a redacted peer note is not an input")


class TestRealAppsFixture(unittest.TestCase):
    """Real two-boot run of experiments/apps_live.py (notes, injection, block, redact, quarantine)."""

    def setUp(self):
        d = os.path.join(os.path.dirname(__file__), "fixtures")
        ps = [os.path.join(d, f"live_apps_boot{i}.log") for i in (1, 2)]
        if not all(os.path.exists(p) for p in ps):
            self.skipTest("fixtures not recorded yet")
        self.b1, self.b2 = (Lineage.from_files([p]) for p in ps)

    def test_injected_note_taints_the_answer_and_taint_query_agrees(self):
        ans = [e for e in self.b1.events.values() if e.kind == "write" and e.key == "answer"]
        self.assertEqual(len(ans), 2)
        self.assertNotEqual(ans[0].attrs.get("taint"), "1")
        self.assertEqual(ans[1].attrs.get("taint"), "1")
        self.assertTrue(self.b1.taint(ans[1].gid)["tainted"], "host-side lineage agrees with the kernel's mark")
        self.assertFalse(self.b1.taint(ans[0].gid)["tainted"])

    def test_block_redact_quarantine(self):
        ev = list(self.b2.events.values())
        self.assertTrue(any(e.kind == "cap_deny" and e.attrs.get("reason") == "tainted_context" for e in ev))
        self.assertTrue(any(e.kind == "read" and e.attrs.get("redacted") == "1" for e in ev))
        self.assertTrue(any(e.kind == "write" and e.key == "n3" and e.attrs.get("removed") == "1" for e in ev))
