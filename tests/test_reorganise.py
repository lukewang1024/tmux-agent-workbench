"""Isolated tmux tests for the reorganisation helper."""

from __future__ import annotations

import json
import os
from pathlib import Path
import pty
import shutil
import subprocess
import tempfile
import time
import unittest


ROOT = Path(__file__).resolve().parents[1]
LAUNCHER = ROOT / "bin" / "wb-reorganise"
TMUX = shutil.which("tmux")


@unittest.skipUnless(TMUX, "tmux is required")
class ReorganiseTmuxTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="wb-reorganise-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.socket = str(self.root / "tmux.sock")
        self.fake_bin = self.root / "bin"
        self.fake_bin.mkdir()
        fake_cli = self.fake_bin / "tmux-agent-workbench"
        fake_cli.write_text("#!/bin/sh\n[ \"$1 $2\" = 'agent list' ] || exit 64\ncat \"$WB_AGENT_LIST_JSON\"\n")
        fake_cli.chmod(0o755)
        self.agent_list = self.root / "agent-list.json"
        self._write_agent_list([])
        tmux_env = dict(os.environ)
        tmux_env.pop("TMUX", None)
        tmux_env.pop("TMUX_PANE", None)
        tmux_env["TERM"] = "xterm"
        # A delayed shell prompt must not change a test placeholder's identity.
        tmux_env["PS1"] = ""
        self.tmux_env = tmux_env
        self.tmux("-f", "/dev/null", "new-session", "-d", "-s", "alpha", "-n", "源✨", "sleep", "600")
        self.tmux("new-window", "-d", "-t", "alpha:", "-n", "shell 🐚", "sleep", "600")
        self.tmux("new-session", "-d", "-s", "beta", "-n", "second", "sleep", "600")
        self.server_pid = self.tmux("display-message", "-p", "#{pid}")
        self.env = dict(
            os.environ,
            PATH=str(self.fake_bin) + os.pathsep + os.environ["PATH"],
            TMUX=f"{self.socket},{self.server_pid},0",
            TMUX_PANE="%0",
            WB_AGENT_LIST_JSON=str(self.agent_list),
            XDG_STATE_HOME=str(self.root / "state"),
        )
        self.addCleanup(self.stop_tmux)

    def stop_tmux(self):
        subprocess.run([TMUX, "-S", self.socket, "kill-server"], env=self.tmux_env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def tmux(self, *args):
        return subprocess.check_output([TMUX, "-S", self.socket, *args], env=self.tmux_env, text=True).rstrip("\n")

    def _write_agent_list(self, agents):
        self.agent_list.write_text(json.dumps({"schema_version": 1, "server": self.socket, "agents": agents}))

    def call(self, *args, ok=True):
        result = subprocess.run([str(LAUNCHER), *args], env=self.env, text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        if ok and result.returncode != 0:
            self.fail(f"helper failed: {result.stderr}\nstdout: {result.stdout}")
        if not ok and result.returncode == 0:
            self.fail(f"helper unexpectedly succeeded: {result.stdout}")
        return result

    def snapshot(self, context_lines=0):
        args = ["snapshot"]
        if context_lines:
            args.extend(["--context-lines", str(context_lines)])
        return json.loads(self.call(*args).stdout)

    def write_json(self, name, value):
        path = self.root / name
        path.write_text(json.dumps(value, ensure_ascii=False, indent=2))
        return path

    def plan_for(self, snapshot, groups, scratchpad=None):
        value = {"groups": groups}
        if scratchpad is not None:
            value["scratchpad"] = scratchpad
        return value

    def apply(self, snapshot, plan):
        snapshot_path = self.write_json("snapshot.json", snapshot)
        plan_path = self.write_json("plan.json", plan)
        result = self.call("apply", "--snapshot", str(snapshot_path), "--plan", str(plan_path))
        return json.loads(result.stdout)

    def panes(self, snapshot):
        return {row["pane_id"]: row for row in snapshot["panes"]}

    def windows(self, snapshot):
        return {row["window_id"]: row for row in snapshot["windows"]}

    def test_snapshot_includes_server_identity_context_and_agent_process_identity(self):
        initial = self.snapshot()
        pane = initial["panes"][0]
        agent = {
            "instance_id": f"{pane['pane_id']}:{pane['pane_pid']}:44:codex",
            "kind": "codex",
            "label": "Build task",
            "thread_name": "implementation",
            "target": {"pane_id": pane["pane_id"], "window_id": pane["window_id"]},
            "process": {"pid": pane["pane_pid"], "started_at_ticks": 44, "executable": "codex"},
        }
        self._write_agent_list([agent])
        second = self.snapshot(context_lines=2)
        with_agent = next(row for row in second["panes"] if row["pane_id"] == pane["pane_id"])
        self.assertEqual(second["server"]["socket_path"], self.socket)
        self.assertEqual(second["server"]["server_pid"], self.server_pid)
        self.assertEqual(with_agent["agent"]["pid"], pane["pane_pid"])
        self.assertEqual(with_agent["agent"]["thread_name"], "implementation")
        self.assertIn("context_tail", with_agent)
        self.assertLessEqual(len(with_agent["context_tail"].splitlines()), 2)
        self.assertTrue(all("context_tail" not in row for row in second["panes"] if row["pane_id"] != pane["pane_id"]))

    def test_unicode_regroup_preserves_window_pane_and_process_identity_and_restore(self):
        before = self.snapshot()
        agent_pane = before["panes"][0]
        self._write_agent_list([{
            "instance_id": f"{agent_pane['pane_id']}:{agent_pane['pane_pid']}:88:codex",
            "kind": "codex",
            "target": {"pane_id": agent_pane["pane_id"], "window_id": agent_pane["window_id"]},
            "process": {"pid": agent_pane["pane_pid"], "started_at_ticks": 88, "executable": "codex"},
        }])
        before = self.snapshot()
        original_groups = {
            session["session_name"]: [w["window_id"] for w in before["windows"] if w["session_id"] == session["session_id"]]
            for session in before["sessions"]
        }
        original_names = {wid: row["window_name"] for wid, row in self.windows(before).items()}
        original_panes = self.panes(before)
        all_ids = [row["window_id"] for row in before["windows"]]
        plan = self.plan_for(before, [{"session": "项目 🧭", "windows": [all_ids[2], all_ids[0], all_ids[1]]}])
        applied = self.apply(before, plan)
        after = self.snapshot()
        self.assertEqual(applied["session_count"], 1)
        self.assertEqual(self.tmux("list-sessions", "-F", "#{session_name}"), "项目 🧭")
        self.assertEqual({row["window_id"] for row in after["windows"]}, set(all_ids))
        self.assertEqual({wid: row["window_name"] for wid, row in self.windows(after).items()}, original_names)
        self.assertEqual({pid: row["pane_pid"] for pid, row in self.panes(after).items()},
                         {pid: row["pane_pid"] for pid, row in original_panes.items()})
        self.assertEqual({pid: row["agent"] for pid, row in self.panes(after).items()},
                         {pid: row["agent"] for pid, row in original_panes.items()})
        self.assertTrue((Path(applied["run_dir"]) / "before.json").is_file())
        self.assertTrue((Path(applied["run_dir"]) / "plan.json").is_file())
        self.assertTrue((Path(applied["run_dir"]) / "after.json").is_file())
        self.assertTrue((Path(applied["run_dir"]) / "journal.jsonl").is_file())
        restored = json.loads(self.call("restore", applied["run_dir"]).stdout)
        self.assertEqual(restored["status"], "complete")
        final = self.snapshot()
        self.assertEqual(self.windows(final).keys(), self.windows(before).keys())
        self.assertEqual({wid: row["window_name"] for wid, row in self.windows(final).items()}, original_names)
        final_groups = {
            session["session_name"]: [w["window_id"] for w in final["windows"] if w["session_id"] == session["session_id"]]
            for session in final["sessions"]
        }
        self.assertEqual(final_groups, original_groups)
        self.assertEqual({pid: row["pane_pid"] for pid, row in self.panes(final).items()},
                         {pid: row["pane_pid"] for pid, row in original_panes.items()})

    def test_scratchpad_session_keeps_original_windows_and_scratch_only_request_creates_owned_shell(self):
        self.tmux("new-session", "-d", "-s", "0", "-n", "old scratch", "sleep", "600")
        snapshot = self.snapshot()
        scratch_window = next(row for row in snapshot["windows"] if row["session_name"] == "0")
        other_ids = [row["window_id"] for row in snapshot["windows"] if row["window_id"] != scratch_window["window_id"]]
        keep_plan = self.plan_for(snapshot, [
            {"session": "0", "windows": [scratch_window["window_id"]]},
            {"session": "工作", "windows": other_ids},
        ], scratchpad="0")
        self.apply(snapshot, keep_plan)
        first_after = self.snapshot()
        zero_windows = [row for row in first_after["windows"] if row["session_name"] == "0"]
        self.assertEqual([row["window_id"] for row in zero_windows], [scratch_window["window_id"]])
        self.assertNotEqual(zero_windows[0]["window_name"], "scratchpad")

        snapshot2 = first_after
        moved_ids = [row["window_id"] for row in snapshot2["windows"]]
        scratch_only_plan = self.plan_for(snapshot2, [{"session": "全体", "windows": moved_ids}], scratchpad="0")
        applied = self.apply(snapshot2, scratch_only_plan)
        after = self.snapshot()
        zero_windows = [row for row in after["windows"] if row["session_name"] == "0"]
        self.assertEqual(len(zero_windows), 1)
        self.assertEqual(zero_windows[0]["window_name"], "scratchpad")
        self.assertNotIn(zero_windows[0]["window_id"], moved_ids)
        self.assertEqual(set(row["window_id"] for row in after["windows"] if row["session_name"] == "全体"), set(moved_ids))
        self.call("restore", applied["run_dir"])
        restored = self.snapshot()
        self.assertEqual({row["window_id"] for row in restored["windows"]}, set(moved_ids))
        self.assertFalse(any(row["window_name"] == "scratchpad" for row in restored["windows"]))

    def test_repeated_noop_has_no_tmux_operations_or_window_churn(self):
        before = self.snapshot()
        ids = [row["window_id"] for row in before["windows"]]
        plan = self.plan_for(before, [
            {"session": "alpha", "windows": [row["window_id"] for row in before["windows"] if row["session_name"] == "alpha"]},
            {"session": "beta", "windows": [row["window_id"] for row in before["windows"] if row["session_name"] == "beta"]},
        ])
        result = self.apply(before, plan)
        second_snapshot = self.snapshot()
        second = self.apply(second_snapshot, plan)
        after = self.snapshot()
        self.assertEqual({row["window_id"] for row in after["windows"]}, set(ids))
        self.assertEqual({row["window_id"] for row in second_snapshot["windows"]}, set(ids))
        journal = Path(second["run_dir"]) / "journal.jsonl"
        self.assertTrue(not journal.exists() or journal.stat().st_size == 0)
        self.assertEqual(result["status"], "complete")

    def _attached_client(self, session):
        master, slave = pty.openpty()
        process = subprocess.Popen([TMUX, "-S", self.socket, "attach-session", "-t", session],
                                   stdin=slave, stdout=slave, stderr=slave, env=self.tmux_env)
        os.close(slave)
        self.addCleanup(lambda: self._stop_client(process, master))
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            clients = self.tmux("list-clients", "-F", "#{client_name} #{session_name}").splitlines()
            match = next((line.split(" ", 1)[0] for line in clients if line.endswith(" " + session)), None)
            if match:
                return match
            time.sleep(0.05)
        self.fail(f"client did not attach to {session}")

    def _stop_client(self, process, master):
        try:
            os.close(master)
        except OSError:
            pass
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=1)
            except subprocess.TimeoutExpired:
                process.kill()

    def test_client_focus_is_preserved_and_conflicting_merge_is_refused(self):
        first_client = self._attached_client("alpha")
        snapshot = self.snapshot()
        focused = next(row for row in snapshot["clients"] if row["client_name"] == first_client)
        plan = self.plan_for(snapshot, [{"session": "合并", "windows": [row["window_id"] for row in snapshot["windows"]]}])
        applied = self.apply(snapshot, plan)
        after = self.snapshot()
        live_client = next(row for row in after["clients"] if row["client_name"] == first_client)
        self.assertEqual(live_client["pane_id"], focused["pane_id"])
        self.assertEqual(live_client["session_name"], "合并")
        self.call("restore", applied["run_dir"])

        second_client = self._attached_client("beta")
        conflict_snapshot = self.snapshot()
        first = next(row for row in conflict_snapshot["clients"] if row["client_name"] == first_client)
        second = next(row for row in conflict_snapshot["clients"] if row["client_name"] == second_client)
        self.assertNotEqual(first["pane_id"], second["pane_id"])
        conflict = self.plan_for(conflict_snapshot, [
            {"session": "same", "windows": [row["window_id"] for row in conflict_snapshot["windows"]]}
        ])
        before_ids = {row["window_id"] for row in conflict_snapshot["windows"]}
        result = self.call("apply", "--snapshot", str(self.write_json("focus.json", conflict_snapshot)),
                           "--plan", str(self.write_json("focus-plan.json", conflict)), ok=False)
        self.assertIn("cannot preserve client focus", result.stderr)
        self.assertEqual({row["window_id"] for row in self.snapshot()["windows"]}, before_ids)

    def test_stale_snapshot_duplicate_and_missing_window_plans_are_rejected_before_moves(self):
        snapshot = self.snapshot()
        ids = [row["window_id"] for row in snapshot["windows"]]
        duplicate = self.plan_for(snapshot, [{"session": "target", "windows": ids + [ids[0]]}])
        result = self.call("apply", "--snapshot", str(self.write_json("duplicate-snapshot.json", snapshot)),
                           "--plan", str(self.write_json("duplicate-plan.json", duplicate)), ok=False)
        self.assertIn("appears more than once", result.stderr)
        missing = self.plan_for(snapshot, [{"session": "target", "windows": ids[:-1]}])
        result = self.call("apply", "--snapshot", str(self.write_json("missing-snapshot.json", snapshot)),
                           "--plan", str(self.write_json("missing-plan.json", missing)), ok=False)
        self.assertIn("omits snapshot window IDs", result.stderr)

        stale = self.snapshot()
        stale_ids = [row["window_id"] for row in stale["windows"]]
        self.tmux("rename-window", "-t", stale_ids[0], "manual change")
        stale_plan = self.plan_for(stale, [{"session": "target", "windows": stale_ids}])
        result = self.call("apply", "--snapshot", str(self.write_json("stale-snapshot.json", stale)),
                           "--plan", str(self.write_json("stale-plan.json", stale_plan)), ok=False)
        self.assertIn("changed since the snapshot", result.stderr)
        current = self.snapshot()
        self.assertEqual({row["session_name"] for row in current["windows"]}, {"alpha", "beta"})
        self.assertEqual(self.windows(current)[stale_ids[0]]["window_name"], "manual change")

    def test_session_group_and_linked_windows_are_refused(self):
        snapshot = self.snapshot()
        self.tmux("new-session", "-d", "-s", "linked")
        self.tmux("link-window", "-s", f"alpha:{snapshot['windows'][0]['window_id']}", "-t", "linked:")
        linked = self.snapshot()
        plan = self.plan_for(linked, [{"session": "target", "windows": list(dict.fromkeys(row["window_id"] for row in linked["windows"]))}])
        result = self.call("apply", "--snapshot", str(self.write_json("linked-snapshot.json", linked)),
                           "--plan", str(self.write_json("linked-plan.json", plan)), ok=False)
        self.assertIn("linked tmux windows are not supported", result.stderr)

        self.tmux("kill-session", "-t", "linked")
        self.tmux("new-session", "-d", "-t", "alpha", "-s", "alpha-peer")
        grouped = self.snapshot()
        plan = self.plan_for(grouped, [{"session": "target", "windows": list(dict.fromkeys(row["window_id"] for row in grouped["windows"]))}])
        result = self.call("apply", "--snapshot", str(self.write_json("grouped-snapshot.json", grouped)),
                           "--plan", str(self.write_json("grouped-plan.json", plan)), ok=False)
        self.assertIn("session groups are not supported", result.stderr)

    def test_restore_refuses_manual_topology_changes(self):
        before = self.snapshot()
        ids = [row["window_id"] for row in before["windows"]]
        applied = self.apply(before, self.plan_for(before, [{"session": "joined", "windows": ids}]))
        extra = self.tmux("new-window", "-d", "-t", "joined:", "-n", "manual", "-P", "-F", "#{window_id}", "sleep", "600")
        result = self.call("restore", applied["run_dir"], ok=False)
        self.assertIn("topology changed since the run", result.stderr)
        after = self.snapshot()
        self.assertIn(extra, {row["window_id"] for row in after["windows"]})

    def test_restore_refuses_a_scratch_shell_that_was_reused(self):
        self.tmux("new-session", "-d", "-s", "0", "-n", "old scratch", "sleep", "600")
        before = self.snapshot()
        ids = [row["window_id"] for row in before["windows"]]
        applied = self.apply(before, self.plan_for(before, [{"session": "work", "windows": ids}], scratchpad="0"))
        after = self.snapshot()
        scratch = next(row for row in after["windows"] if row["session_name"] == "0")
        pane_id = scratch["pane_ids"][0]
        expected_path = Path("/tmp").resolve()
        self.tmux("send-keys", "-t", pane_id, "cd /tmp", "Enter")
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            if Path(self.tmux("display-message", "-p", "-t", pane_id, "#{pane_current_path}")).resolve() == expected_path:
                break
            time.sleep(0.05)
        result = self.call("restore", applied["run_dir"], ok=False)
        self.assertIn("scratch shell changed", result.stderr)
        self.assertEqual(Path(self.tmux("display-message", "-p", "-t", pane_id, "#{pane_current_path}")).resolve(), expected_path)

    def test_restore_preserves_a_client_viewing_the_created_scratch_shell(self):
        before = self.snapshot()
        ids = [row["window_id"] for row in before["windows"]]
        applied = self.apply(before, self.plan_for(before, [{"session": "work", "windows": ids}], scratchpad="0"))
        client = self._attached_client("0")
        current = self.snapshot()
        focused = next(row for row in current["clients"] if row["client_name"] == client)
        result = self.call("restore", applied["run_dir"], ok=False)
        self.assertIn("scratch shell is being viewed", result.stderr)
        after = self.snapshot()
        self.assertEqual({row["window_id"] for row in after["windows"]},
                         {row["window_id"] for row in current["windows"]})
        self.assertEqual(next(row for row in after["clients"] if row["client_name"] == client)["pane_id"],
                         focused["pane_id"])


class BatchWorkerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="wb-batch-worker-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        script = self.bin / "agent-team"
        script.write_text("#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$ARGS_LOG\"\ncat \"$RESOLVER_JSON\"\n")
        script.chmod(0o755)
        self.payload = self.root / "resolver.json"
        self.args_log = self.root / "args.txt"
        self.env = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
                        ARGS_LOG=str(self.args_log), RESOLVER_JSON=str(self.payload))

    def test_reads_batch_worker_settings_without_launching_returned_argv(self):
        self.payload.write_text(json.dumps({
            "config": {"scene": "batch", "worker_model": "gpt-6-luna", "worker_effort": "max"},
            "argv": ["this", "must", "never", "launch"],
        }))
        result = subprocess.run([str(LAUNCHER), "batch-worker"], env=self.env, text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True)
        self.assertEqual(json.loads(result.stdout), {
            "scene": "batch", "model": "gpt-6-luna", "reasoning_effort": "max"
        })
        self.assertEqual(self.args_log.read_text().splitlines(), [
            "start", "codex", "--scene", "batch", "--team", "--team-dry-run"
        ])

    def test_missing_batch_settings_fail_clearly(self):
        self.payload.write_text(json.dumps({"config": {"scene": "batch", "worker_model": "gpt-6-luna"}}))
        result = subprocess.run([str(LAUNCHER), "batch-worker"], env=self.env, text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("config.worker_effort is missing", result.stderr)


if __name__ == "__main__":
    unittest.main()
