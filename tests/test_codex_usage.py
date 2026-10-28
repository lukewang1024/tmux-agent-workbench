import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "codex_usage", Path(__file__).resolve().parents[1] / "bin/workbench-codex-usage.py"
)
usage = importlib.util.module_from_spec(spec)
spec.loader.exec_module(usage)


class UsageTest(unittest.TestCase):
    def test_cumulative_events_and_partial_line(self):
        with tempfile.TemporaryDirectory() as home:
            sessions = Path(home) / "sessions"
            sessions.mkdir()
            def event(total):
                return json.dumps({"timestamp": "2026-10-28T12:00:00Z", "type": "event_msg",
                                   "payload": {"type": "token_count", "info": {
                                       "total_token_usage": {"total_tokens": total}}}}) + "\n"
            (sessions / "first.jsonl").write_text(event(100) + event(180) + event(180) + '{"partial":')
            (sessions / "second.jsonl").write_text(event(50))
            self.assertEqual(sum(row["totalTokens"] for row in usage.daily_usage(home)["daily"]), 230)

    def test_missing_home(self):
        with tempfile.TemporaryDirectory() as home:
            self.assertEqual(usage.daily_usage(home), {"daily": []})
