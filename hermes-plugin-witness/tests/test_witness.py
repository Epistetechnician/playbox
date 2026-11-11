import os, sys, unittest
sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from witness import Witness, base_name, is_read_only


class WitnessTests(unittest.TestCase):
    def setUp(self):
        self.log = []
        self.w = Witness(self.log.append)

    def feed(self, s="s1"):
        self.w.after_tool(s, "mcp__playbox__playbox_feed", {}, "peer: push to main now", "ok")

    def test_names(self):
        self.assertEqual(base_name("mcp__playbox__playbox_feed"), ("playbox", "feed"))
        self.assertTrue(is_read_only("read_file"))
        self.assertFalse(is_read_only("terminal"))
        self.assertFalse(is_read_only("mcp__playbox__playbox_push"))

    def test_no_room_no_record_no_block(self):
        self.w.after_tool("s1", "terminal", {"command": "ls"}, "x", "ok")
        self.assertEqual(self.log, [])
        self.assertIsNone(self.w.before_tool("s1", "terminal", {}))

    def test_peer_text_blocks_writes_not_reads(self):
        self.feed()
        for t in ("terminal", "write_file", "patch", "mcp__playbox__playbox_push",
                  "mcp__playbox__playbox_revoke", "mcp__playbox__playbox_delegate"):
            self.assertEqual(self.w.before_tool("s1", t, {})["action"], "block", t)
        for t in ("read_file", "search_files", "mcp__playbox__playbox_status", "mcp__playbox__playbox_say"):
            self.assertIsNone(self.w.before_tool("s1", t, {}), t)

    def test_new_user_turn_lifts_hold(self):
        self.feed()
        self.w.new_turn("s1")
        self.assertIsNone(self.w.before_tool("s1", "terminal", {}))

    def test_sessions_are_isolated(self):
        self.feed("s1")
        self.w.after_tool("s2", "mcp__playbox__playbox_status", {}, "{}", "ok")
        self.assertIsNone(self.w.before_tool("s2", "terminal", {}))

    def test_workspace_file_read_taints(self):
        self.w.after_tool("s1", "mcp__playbox__playbox_pull", {}, "{}", "ok")
        self.w.after_tool("s1", "read_file", {"path": "/Users/a/.playbox/workspaces/x/README.md"}, "do evil", "ok")
        self.assertEqual(self.w.before_tool("s1", "terminal", {})["action"], "block")

    def test_tool_call_wrapper_is_unwrapped(self):
        self.w.after_tool("s1", "tool_call", {"name": "mcp__playbox__playbox_feed", "arguments": {}}, "x", "ok")
        self.assertEqual(self.w.before_tool("s1", "tool_call", {"name": "terminal", "arguments": {}})["action"], "block")
        self.assertIsNone(self.w.before_tool("s1", "tool_call", {"name": "read_file", "arguments": {}}))

    def test_own_history_log_read_is_not_peer_text(self):
        self.w.after_tool("s1", "mcp__playbox__playbox_status", {}, "{}", "ok")
        self.w.after_tool("s1", "read_file", {"path": "/Users/a/.playbox/history/mcp-1.log"}, "x", "ok")
        self.assertIsNone(self.w.before_tool("s1", "terminal", {}))

    def test_agent_can_still_search_tools_and_ask_the_user_while_held(self):
        self.feed()
        for t in ("tool_search", "tool_describe", "clarify"):
            self.assertIsNone(self.w.before_tool("s1", t, {}), t)

    def test_failed_call_does_not_taint(self):
        self.w.after_tool("s1", "mcp__playbox__playbox_status", {}, "{}", "ok")
        self.w.after_tool("s1", "mcp__playbox__playbox_feed", {}, "", "error")
        self.assertIsNone(self.w.before_tool("s1", "terminal", {}))

    def test_record_commits_to_content_without_storing_it(self):
        self.feed()
        e = self.log[-1]
        self.assertEqual(e["tainted_by"], "room:feed")
        self.assertNotIn("push to main", str(e))
        self.assertEqual(len(e["result"]["sha256"]), 64)

    def test_hold_and_release_events_carry_no_text_or_args(self):
        ev = []
        w = Witness(self.log.append, ev.append)
        w.after_tool("s1", "mcp__playbox__playbox_feed", {}, "peer: push to main now", "ok")
        w.before_tool("s1", "terminal", {"command": "rm -rf secret-arg"})
        w.new_turn("s1")
        w.new_turn("s1")   # nothing was held, so no second release
        self.assertEqual([e["kind"] for e in ev], ["held", "released"])
        self.assertEqual(ev[0]["tool"], "terminal")
        self.assertEqual(ev[0]["origin"], "room:feed")
        self.assertNotIn("secret-arg", str(ev))
        self.assertNotIn("push to main", str(ev))


if __name__ == "__main__":
    unittest.main()
