#!/usr/bin/env python3
"""Deterministic, process-preserving tmux session/window reorganisation."""

from __future__ import annotations

import argparse
import datetime as _datetime
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import uuid
from typing import Any


class ReorganiseError(Exception):
    pass


def _json_bytes(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n").encode("utf-8")


def _read_json(path: str | Path) -> Any:
    try:
        with open(path, "r", encoding="utf-8") as stream:
            return json.load(stream)
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise ReorganiseError(f"cannot read JSON file {path}: {exc}") from exc


def _write_private(path: Path, value: Any) -> None:
    data = _json_bytes(value)
    flags = os.O_WRONLY | os.O_CREAT | os.O_TRUNC
    fd = os.open(path, flags, 0o600)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
    except BaseException:
        try:
            os.close(fd)
        except OSError:
            pass
        raise
    os.chmod(path, 0o600)


def _private_dir(path: Path) -> None:
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(path, 0o700)


def _run_id() -> str:
    stamp = _datetime.datetime.now(_datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    return f"{stamp}-{uuid.uuid4().hex[:10]}"


def _tmux_origin() -> tuple[str, str, str]:
    raw = os.environ.get("TMUX", "")
    parts = raw.rsplit(",", 2)
    if len(parts) != 3 or not parts[0] or not parts[1].isdigit() or not parts[2].isdigit():
        raise ReorganiseError("TMUX must identify the current tmux server (socket, server PID, session index)")
    return parts[0], parts[1], parts[2]


class Tmux:
    def __init__(self, socket: str, server_pid: str):
        self.socket = socket
        self.server_pid = server_pid

    def call(self, *args: str, check: bool = True) -> str:
        try:
            result = subprocess.run(
                ["tmux", "-S", self.socket, *map(str, args)],
                check=False,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                encoding="utf-8",
                errors="replace",
            )
        except OSError as exc:
            raise ReorganiseError(f"cannot run tmux: {exc}") from exc
        if check and result.returncode != 0:
            detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
            raise ReorganiseError(f"tmux {' '.join(map(str, args))} failed: {detail}")
        return result.stdout.rstrip("\n")

    def lines(self, *args: str) -> list[str]:
        output = self.call(*args)
        return output.splitlines() if output else []

    def server_identity(self) -> dict[str, str]:
        value = self.call("display-message", "-p", "#{socket_path}\t#{pid}")
        try:
            socket, pid = value.split("\t", 1)
        except ValueError as exc:
            raise ReorganiseError("tmux did not report its socket and server PID") from exc
        if socket != self.socket:
            raise ReorganiseError(f"TMUX socket mismatch: expected {self.socket!r}, connected to {socket!r}")
        if pid != self.server_pid:
            raise ReorganiseError(
                f"TMUX server PID mismatch: environment says {self.server_pid}, server is {pid}"
            )
        return {"socket_path": socket, "server_pid": pid}


def _format(tmux: Tmux, target: str, fmt: str) -> str:
    return tmux.call("display-message", "-p", "-t", target, fmt)


def _agent_list(socket: str) -> dict[str, dict[str, Any]]:
    try:
        result = subprocess.run(
            ["tmux-agent-workbench", "agent", "list"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            errors="replace",
        )
    except OSError as exc:
        raise ReorganiseError(f"cannot run public agent list command: {exc}") from exc
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise ReorganiseError(f"public command 'tmux-agent-workbench agent list' failed: {detail}")
    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        raise ReorganiseError("public agent list command did not return JSON") from exc
    if not isinstance(payload, dict) or not isinstance(payload.get("agents"), list):
        raise ReorganiseError("public agent list JSON is missing its agents array")
    reported_server = payload.get("server")
    if reported_server != socket:
        raise ReorganiseError(
            f"public agent list server mismatch: expected {socket!r}, got {reported_server!r}"
        )
    identities: dict[str, dict[str, Any]] = {}
    for row in payload["agents"]:
        if not isinstance(row, dict):
            continue
        target = row.get("target")
        process = row.get("process")
        if not isinstance(target, dict) or not isinstance(process, dict):
            continue
        pane_id = target.get("pane_id")
        if not isinstance(pane_id, str) or not pane_id.startswith("%"):
            continue
        identity = {
            "instance_id": row.get("instance_id"),
            "kind": row.get("kind"),
            "window_id": target.get("window_id"),
            "pane_id": pane_id,
            "pid": process.get("pid"),
            "started_at_ticks": process.get("started_at_ticks"),
            "executable": process.get("executable"),
            "thread_name": row.get("thread_name"),
            "label": row.get("label"),
        }
        if pane_id in identities:
            raise ReorganiseError(f"agent list returned more than one agent for pane {pane_id}")
        identities[pane_id] = identity
    return identities


def _capture_tail(tmux: Tmux, pane_id: str, line_count: int) -> str:
    if line_count == 0:
        return ""
    return tmux.call("capture-pane", "-p", "-S", f"-{line_count}", "-E", "-1", "-t", pane_id)


def take_snapshot(context_lines: int = 0) -> dict[str, Any]:
    if context_lines < 0 or context_lines > 20:
        raise ReorganiseError("--context-lines must be between 0 and 20")
    socket, server_pid, origin_index = _tmux_origin()
    tmux = Tmux(socket, server_pid)
    server = tmux.server_identity()
    server["origin_session_index"] = origin_index
    agent_by_pane = _agent_list(socket)

    sessions: list[dict[str, Any]] = []
    window_rows: list[dict[str, Any]] = []
    pane_rows: dict[str, dict[str, Any]] = {}
    for session_id in tmux.lines("list-sessions", "-F", "#{session_id}"):
        if not session_id.startswith("$"):
            raise ReorganiseError(f"tmux returned invalid session id {session_id!r}")
        session_name = _format(tmux, session_id, "#{session_name}")
        group = _format(tmux, session_id, "#{session_group}") or None
        active_window = _format(tmux, session_id, "#{window_id}")
        active_pane = _format(tmux, session_id, "#{pane_id}")
        sessions.append({
            "session_id": session_id,
            "session_name": session_name,
            "session_group": group,
            "attached": int(_format(tmux, session_id, "#{session_attached}") or "0"),
            "window_count": int(_format(tmux, session_id, "#{session_windows}") or "0"),
            "active_window_id": active_window or None,
            "active_pane_id": active_pane or None,
        })
        for window_id in tmux.lines("list-windows", "-t", session_id, "-F", "#{window_id}"):
            index = int(_format(tmux, f"{session_id}:{window_id}", "#{window_index}"))
            name = _format(tmux, f"{session_id}:{window_id}", "#{window_name}")
            active = _format(tmux, f"{session_id}:{window_id}", "#{window_active}") == "1"
            pane_ids = tmux.lines("list-panes", "-t", f"{session_id}:{window_id}", "-F", "#{pane_id}")
            active_pane_id = _format(tmux, f"{session_id}:{window_id}", "#{pane_id}")
            row = {
                "window_id": window_id,
                "session_id": session_id,
                "session_name": session_name,
                "window_index": index,
                "window_name": name,
                "active": active,
                "active_pane_id": active_pane_id or None,
                "pane_ids": pane_ids,
            }
            window_rows.append(row)
            for pane_id in pane_ids:
                if pane_id in pane_rows:
                    continue
                pane = {
                    "pane_id": pane_id,
                    "window_id": window_id,
                    "session_id": session_id,
                    "pane_index": int(_format(tmux, pane_id, "#{pane_index}")),
                    "pane_pid": int(_format(tmux, pane_id, "#{pane_pid}") or "0"),
                    "current_path": _format(tmux, pane_id, "#{pane_current_path}"),
                    "current_command": _format(tmux, pane_id, "#{pane_current_command}"),
                    "dead": _format(tmux, pane_id, "#{pane_dead}") == "1",
                    "agent": agent_by_pane.get(pane_id),
                }
                if context_lines and pane["agent"]:
                    pane["context_tail"] = _capture_tail(tmux, pane_id, context_lines)
                pane_rows[pane_id] = pane

    occurrence_count: dict[str, int] = {}
    for row in window_rows:
        occurrence_count[row["window_id"]] = occurrence_count.get(row["window_id"], 0) + 1
    for row in window_rows:
        row["linked"] = occurrence_count[row["window_id"]] > 1

    clients: list[dict[str, Any]] = []
    for row in tmux.lines(
        "list-clients", "-F",
        "#{client_name}\t#{session_id}\t#{session_name}\t#{window_id}\t#{pane_id}",
    ):
        try:
            client_name, session_id, session_name, window_id, pane_id = row.split("\t", 4)
        except ValueError as exc:
            raise ReorganiseError("tmux returned a malformed client row") from exc
        clients.append({
            "client_name": client_name,
            "session_id": session_id,
            "session_name": session_name,
            "window_id": window_id,
            "pane_id": pane_id,
        })

    return {
        "schema_version": 1,
        "observed_at": _datetime.datetime.now(_datetime.timezone.utc).isoformat(),
        "server": server,
        "sessions": sessions,
        "windows": window_rows,
        "panes": list(pane_rows.values()),
        "clients": clients,
        "agent_identities": sorted(agent_by_pane.values(), key=lambda item: item["pane_id"]),
    }


def _agent_signature(snapshot: dict[str, Any]) -> tuple[Any, ...]:
    return tuple(
        sorted(
            (
                row.get("pane_id"),
                row.get("instance_id"),
                row.get("kind"),
                row.get("window_id"),
                row.get("pid"),
                row.get("started_at_ticks"),
                row.get("executable"),
            )
            for row in snapshot.get("agent_identities", [])
        )
    )


def _topology_signature(snapshot: dict[str, Any], *, include_clients: bool = True) -> tuple[Any, ...]:
    server = snapshot.get("server") or {}
    sessions = tuple(sorted(
        (
            row.get("session_id"), row.get("session_name"), row.get("session_group"),
            row.get("window_count"),
        )
        for row in snapshot.get("sessions", [])
    ))
    windows = tuple(sorted(
        (
            row.get("window_id"), row.get("session_id"), row.get("session_name"),
            row.get("window_index"), row.get("window_name"), row.get("linked"),
            tuple(row.get("pane_ids", [])),
        )
        for row in snapshot.get("windows", [])
    ))
    panes = tuple(sorted(
        (
            row.get("pane_id"), row.get("window_id"), row.get("pane_pid"),
            row.get("current_path"), row.get("dead"),
            _identity_tuple(row.get("agent")),
        )
        for row in snapshot.get("panes", [])
    ))
    clients = ()
    if include_clients:
        clients = tuple(sorted(
            (row.get("client_name"), row.get("session_id"), row.get("window_id"), row.get("pane_id"))
            for row in snapshot.get("clients", [])
        ))
    return (
        server.get("socket_path"), server.get("server_pid"), sessions, windows, panes,
        _agent_signature(snapshot), clients,
    )


def _identity_tuple(identity: Any) -> tuple[Any, ...] | None:
    if not isinstance(identity, dict):
        return None
    return tuple(identity.get(key) for key in (
        "instance_id", "kind", "window_id", "pane_id", "pid", "started_at_ticks", "executable"
    ))


def _check_fresh(expected: dict[str, Any], current: dict[str, Any], *, include_clients: bool = True) -> None:
    if _topology_signature(expected, include_clients=include_clients) != _topology_signature(
        current, include_clients=include_clients
    ):
        raise ReorganiseError(
            "tmux topology or process identity changed since the snapshot; take a fresh snapshot and rebuild the plan"
        )


def _check_restore_fresh(expected: dict[str, Any], current: dict[str, Any]) -> None:
    expected_server = expected.get("server") or {}
    current_server = current.get("server") or {}
    if (expected_server.get("socket_path"), expected_server.get("server_pid")) != (
        current_server.get("socket_path"), current_server.get("server_pid")
    ):
        raise ReorganiseError("run belongs to a different tmux server")
    expected_windows = tuple(sorted(
        (row.get("window_id"), row.get("session_id"), row.get("session_name"),
         row.get("window_index"), row.get("window_name"), row.get("linked"), tuple(row.get("pane_ids", [])))
        for row in expected.get("windows", [])
    ))
    current_windows = tuple(sorted(
        (row.get("window_id"), row.get("session_id"), row.get("session_name"),
         row.get("window_index"), row.get("window_name"), row.get("linked"), tuple(row.get("pane_ids", [])))
        for row in current.get("windows", [])
    ))
    expected_sessions = tuple(sorted(
        (row.get("session_id"), row.get("session_name"), row.get("session_group"), row.get("window_count"))
        for row in expected.get("sessions", [])
    ))
    current_sessions = tuple(sorted(
        (row.get("session_id"), row.get("session_name"), row.get("session_group"), row.get("window_count"))
        for row in current.get("sessions", [])
    ))
    if expected_windows != current_windows or expected_sessions != current_sessions:
        raise ReorganiseError("tmux topology changed since the run; refusing to overwrite manual changes")
    expected_panes = {row.get("pane_id"): row for row in expected.get("panes", [])}
    current_panes = {row.get("pane_id"): row for row in current.get("panes", [])}
    if expected_panes.keys() != current_panes.keys():
        raise ReorganiseError("pane set changed since the run; refusing to overwrite manual changes")
    for pane_id, row in expected_panes.items():
        live = current_panes[pane_id]
        if row.get("pane_pid") != live.get("pane_pid") or row.get("dead") != live.get("dead"):
            raise ReorganiseError(f"pane process identity changed for {pane_id}; refusing to overwrite manual changes")
        if _identity_tuple(row.get("agent")) != _identity_tuple(live.get("agent")):
            raise ReorganiseError(f"agent process identity changed for {pane_id}; refusing to overwrite manual changes")


def _ensure_unlinked_ungrouped(snapshot: dict[str, Any]) -> None:
    grouped = [row["session_name"] for row in snapshot.get("sessions", []) if row.get("session_group")]
    if grouped:
        raise ReorganiseError(
            "tmux session groups are not supported; ungroup these sessions before reorganising: "
            + ", ".join(sorted(set(grouped)))
        )
    if any(row.get("linked") for row in snapshot.get("windows", [])):
        raise ReorganiseError("linked tmux windows are not supported; unlink them before reorganising")
    ids = [row.get("window_id") for row in snapshot.get("windows", [])]
    if len(ids) != len(set(ids)):
        raise ReorganiseError("snapshot has ambiguous duplicate window IDs")


def _validate_session_name(value: Any, field: str = "session") -> str:
    if not isinstance(value, str) or not value:
        raise ReorganiseError(f"{field} must be a non-empty tmux session name")
    if value in {".", ".."} or ":" in value or any(ord(ch) < 32 or ord(ch) == 127 for ch in value):
        raise ReorganiseError(f"invalid {field} name {value!r}; '.', colons, and control characters are not allowed")
    return value


def _validate_plan(snapshot: dict[str, Any], plan: Any) -> dict[str, Any]:
    if not isinstance(plan, dict) or not isinstance(plan.get("groups"), list) or not plan["groups"]:
        raise ReorganiseError("plan must be an object with a non-empty groups array")
    original_ids = [row["window_id"] for row in snapshot.get("windows", [])]
    if len(original_ids) != len(set(original_ids)):
        raise ReorganiseError("snapshot contains duplicate window IDs")
    original_set = set(original_ids)
    seen: set[str] = set()
    target_names: set[str] = set()
    groups: list[dict[str, Any]] = []
    for i, group in enumerate(plan["groups"]):
        if not isinstance(group, dict):
            raise ReorganiseError(f"groups[{i}] must be an object")
        name = _validate_session_name(group.get("session"), f"groups[{i}].session")
        if name in target_names:
            raise ReorganiseError(f"duplicate destination session {name!r}")
        target_names.add(name)
        windows = group.get("windows")
        if not isinstance(windows, list) or not windows:
            raise ReorganiseError(f"groups[{i}].windows must be a non-empty array")
        normalized: list[str] = []
        for window_id in windows:
            if not isinstance(window_id, str) or not window_id.startswith("@"):
                raise ReorganiseError(f"groups[{i}] contains invalid window ID {window_id!r}")
            if window_id in seen:
                raise ReorganiseError(f"window {window_id} appears more than once in the plan")
            seen.add(window_id)
            normalized.append(window_id)
        groups.append({"session": name, "windows": normalized})
    extra = seen - original_set
    missing = original_set - seen
    if extra:
        raise ReorganiseError("plan contains window IDs absent from the snapshot: " + ", ".join(sorted(extra)))
    if missing:
        raise ReorganiseError("plan omits snapshot window IDs: " + ", ".join(sorted(missing)))
    scratch = plan.get("scratchpad")
    if scratch is not None:
        scratch = _validate_session_name(scratch, "scratchpad")
    return {"groups": groups, "scratchpad": scratch}


def _window_to_group(plan: dict[str, Any]) -> dict[str, str]:
    return {window_id: group["session"] for group in plan["groups"] for window_id in group["windows"]}


def _focus_conflict(snapshot: dict[str, Any], window_to_group: dict[str, str]) -> None:
    focused: dict[str, tuple[str, str]] = {}
    for client in snapshot.get("clients", []):
        pane_id = client.get("pane_id")
        window_id = client.get("window_id")
        if not pane_id or not window_id:
            continue
        destination = window_to_group.get(window_id)
        if destination is None:
            continue
        previous = focused.get(destination)
        if previous and previous[1] != pane_id:
            raise ReorganiseError(
                f"cannot preserve client focus: clients viewing different panes {previous[1]} and {pane_id} "
                f"would share destination session {destination!r}"
            )
        focused[destination] = (client.get("client_name", ""), pane_id)


def _sid_by_name(snapshot: dict[str, Any]) -> dict[str, str]:
    result: dict[str, str] = {}
    for row in snapshot.get("sessions", []):
        name = row["session_name"]
        if name in result:
            raise ReorganiseError(f"snapshot contains ambiguous session name {name!r}")
        result[name] = row["session_id"]
    return result


class Journal:
    def __init__(self, run_dir: Path):
        self.path = run_dir / "journal.jsonl"

    def append(self, event: dict[str, Any]) -> None:
        fd = os.open(self.path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
        try:
            with os.fdopen(fd, "wb") as stream:
                line = json.dumps(event, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
                stream.write((line + "\n").encode("utf-8"))
                stream.flush()
                os.fsync(stream.fileno())
        except BaseException:
            try:
                os.close(fd)
            except OSError:
                pass
            raise
        os.chmod(self.path, 0o600)


class Mutator:
    def __init__(self, tmux: Tmux, journal: Journal):
        self.tmux = tmux
        self.journal = journal
        self.sequence = 0

    def command(self, label: str, *args: str) -> str:
        self.sequence += 1
        op_id = self.sequence
        self.journal.append({"id": op_id, "label": label, "status": "started", "argv": ["tmux", *args]})
        try:
            output = self.tmux.call(*args)
        except ReorganiseError as exc:
            self.journal.append({"id": op_id, "label": label, "status": "failed", "error": str(exc)})
            raise
        self.journal.append({"id": op_id, "label": label, "status": "complete"})
        return output


def _write_run_manifest(run_dir: Path, manifest: dict[str, Any]) -> None:
    temporary = run_dir / "run.json.tmp"
    _write_private(temporary, manifest)
    os.replace(temporary, run_dir / "run.json")
    os.chmod(run_dir / "run.json", 0o600)


def _new_session(mutator: Mutator, name: str, cwd: str | None, *, label: str) -> tuple[str, str]:
    args = ["new-session", "-d", "-s", name]
    if cwd and os.path.isdir(cwd):
        args.extend(["-c", cwd])
    args.extend([
        "-n", "scratchpad" if label == "scratch" else "__wb_reorganise__",
        "-P", "-F", "#{session_id}\t#{window_id}", "/bin/sh",
    ])
    output = mutator.command(f"create {label} session {name}", *args)
    try:
        session_id, window_id = output.split("\t", 1)
    except ValueError as exc:
        raise ReorganiseError(f"tmux did not report IDs for created session {name!r}") from exc
    mutator.command(
        f"disable automatic rename for placeholder {window_id}",
        "set-window-option", "-t", window_id, "automatic-rename", "off",
    )
    return session_id, window_id


def _new_placeholder(mutator: Mutator, session_id: str, name: str, cwd: str | None, *, label: str) -> str:
    args = ["new-window", "-d", "-t", f"{session_id}:"]
    if cwd and os.path.isdir(cwd):
        args.extend(["-c", cwd])
    args.extend([
        "-n", "scratchpad" if label == "scratch" else "__wb_reorganise__",
        "-P", "-F", "#{window_id}", "/bin/sh",
    ])
    window_id = mutator.command(f"create {label} placeholder in {name}", *args)
    mutator.command(f"disable automatic rename for placeholder {window_id}", "set-window-option", "-t", window_id, "automatic-rename", "off")
    return window_id


def _placeholder_identity(tmux: Tmux, window_id: str) -> dict[str, Any]:
    panes = tmux.lines("list-panes", "-t", window_id, "-F", "#{pane_id}")
    pane_rows = []
    for pane in panes:
        visible = tmux.call("capture-pane", "-p", "-t", pane)
        visible_lines = [line.rstrip() for line in visible.splitlines()]
        while visible_lines and not visible_lines[-1]:
            visible_lines.pop()
        visible = "\n".join(visible_lines)
        pane_rows.append({
            "pane_id": pane,
            "pane_pid": int(_format(tmux, pane, "#{pane_pid}") or "0"),
            "current_command": _format(tmux, pane, "#{pane_current_command}"),
            "current_path": _format(tmux, pane, "#{pane_current_path}"),
            "history_size": int(_format(tmux, pane, "#{history_size}") or "0"),
            "visible_sha256": hashlib.sha256(visible.encode("utf-8", "replace")).hexdigest(),
        })
    return {
        "window_id": window_id,
        "window_name": _format(tmux, window_id, "#{window_name}"),
        "panes": pane_rows,
    }


def _placeholder_matches(tmux: Tmux, identity: dict[str, Any]) -> bool:
    window_id = identity.get("window_id")
    try:
        current = _placeholder_identity(tmux, window_id)
    except ReorganiseError:
        return False
    return current == {
        "window_id": identity.get("window_id"),
        "window_name": identity.get("window_name"),
        "panes": identity.get("panes"),
    }


def _cwd_for_group(snapshot: dict[str, Any], window_ids: list[str]) -> str | None:
    panes = {row["pane_id"]: row for row in snapshot.get("panes", [])}
    for window_id in window_ids:
        window = next((row for row in snapshot.get("windows", []) if row["window_id"] == window_id), None)
        if window:
            for pane_id in window.get("pane_ids", []):
                path = panes.get(pane_id, {}).get("current_path")
                if path and os.path.isdir(path):
                    return path
    return None


def _base_index(tmux: Tmux, session_id: str) -> int:
    value = tmux.call("show-options", "-t", session_id, "-qv", "base-index", check=False)
    if not value:
        value = tmux.call("show-options", "-gqv", "base-index", check=False)
    try:
        return int(value) if value else 0
    except ValueError:
        return 0


def _index_of_window(tmux: Tmux, window_id: str) -> tuple[str, int]:
    session_id = _format(tmux, window_id, "#{session_id}")
    index = int(_format(tmux, window_id, "#{window_index}"))
    return session_id, index


def _window_at(tmux: Tmux, session_id: str, index: int) -> str | None:
    for row in tmux.lines("list-windows", "-t", session_id, "-F", "#{window_index}\t#{window_id}"):
        try:
            current_index, window_id = row.split("\t", 1)
            if int(current_index) == index:
                return window_id
        except ValueError:
            continue
    return None


def _order_group(mutator: Mutator, session_id: str, desired: list[str], placeholder_id: str | None) -> None:
    base = _base_index(mutator.tmux, session_id)
    for offset, window_id in enumerate(desired):
        target_index = base + offset
        current_session, current_index = _index_of_window(mutator.tmux, window_id)
        if current_session != session_id:
            raise ReorganiseError(f"window {window_id} did not reach destination session")
        if current_index == target_index:
            continue
        occupied = _window_at(mutator.tmux, session_id, target_index)
        if occupied:
            mutator.command(
                f"order {window_id} at index {target_index}",
                "swap-window", "-s", f"{session_id}:{current_index}", "-t", f"{session_id}:{target_index}",
            )
        else:
            mutator.command(
                f"order {window_id} at index {target_index}",
                "move-window", "-s", window_id, "-t", f"{session_id}:{target_index}",
            )
    if placeholder_id:
        target = base + len(desired)
        _, current_index = _index_of_window(mutator.tmux, placeholder_id)
        if current_index != target:
            occupied = _window_at(mutator.tmux, session_id, target)
            if occupied:
                raise ReorganiseError(f"placeholder destination index {target} is unexpectedly occupied")
            mutator.command(
                f"place placeholder {placeholder_id} after requested windows",
                "move-window", "-s", placeholder_id, "-t", f"{session_id}:{target}",
            )


def _prepare_client_sessions(
    mutator: Mutator,
    clients: list[dict[str, Any]],
    window_to_group: dict[str, str],
    session_ids: dict[str, str],
) -> None:
    for client in clients:
        client_name = client.get("client_name")
        destination = window_to_group.get(client.get("window_id"))
        if not client_name or not destination:
            continue
        target_session = session_ids[destination]
        if client.get("session_id") == target_session:
            continue
        mutator.command(
            f"keep client {client_name} attached while moving its window",
            "switch-client", "-c", client_name, "-t", target_session,
        )


def _locate_clients(mutator: Mutator, clients: list[dict[str, Any]]) -> None:
    for client in clients:
        pane_id = client.get("pane_id")
        client_name = client.get("client_name")
        if not pane_id or not client_name:
            continue
        try:
            session_id = _format(mutator.tmux, pane_id, "#{session_id}")
            window_id = _format(mutator.tmux, pane_id, "#{window_id}")
            mutator.command(f"restore client {client_name} session", "switch-client", "-c", client_name, "-t", session_id)
            mutator.command(f"restore client {client_name} window", "select-window", "-t", f"{session_id}:{window_id}")
            mutator.command(f"restore client {client_name} pane", "select-pane", "-t", pane_id)
        except ReorganiseError as exc:
            raise ReorganiseError(f"could not restore client {client_name!r} to pane {pane_id}: {exc}") from exc


def _safe_cleanup_placeholder(mutator: Mutator, identity: dict[str, Any]) -> None:
    if not _placeholder_matches(mutator.tmux, identity):
        raise ReorganiseError(
            f"task-owned placeholder {identity.get('window_id')} changed; leaving it in place for safety"
        )
    mutator.command("remove task-owned placeholder " + identity["window_id"], "kill-window", "-t", identity["window_id"])


def _snapshot_expected_groups(snapshot: dict[str, Any]) -> dict[str, list[str]]:
    groups: dict[str, list[dict[str, Any]]] = {}
    for window in snapshot.get("windows", []):
        groups.setdefault(window["session_name"], []).append(window)
    return {
        name: [row["window_id"] for row in sorted(rows, key=lambda row: (row["window_index"], row["window_id"]))]
        for name, rows in groups.items()
    }


def _current_groups(snapshot: dict[str, Any]) -> dict[str, list[str]]:
    groups: dict[str, list[dict[str, Any]]] = {}
    for window in snapshot.get("windows", []):
        groups.setdefault(window["session_name"], []).append(window)
    return {
        name: [row["window_id"] for row in sorted(rows, key=lambda row: (row["window_index"], row["window_id"]))]
        for name, rows in groups.items()
    }


def _verify_apply(snapshot: dict[str, Any], plan: dict[str, Any], after: dict[str, Any], scratch_shell: dict[str, Any] | None) -> None:
    expected = {group["session"]: group["windows"][:] for group in plan["groups"]}
    if plan.get("scratchpad") and plan["scratchpad"] not in expected:
        expected[plan["scratchpad"]] = [scratch_shell["window_id"]] if scratch_shell else []
    if _current_groups(after) != expected:
        raise ReorganiseError("after verification found unexpected session window grouping or order")
    after_by_id = {row["pane_id"]: row for row in after.get("panes", [])}
    for row in snapshot.get("panes", []):
        current = after_by_id.get(row["pane_id"])
        if not current or current.get("pane_pid") != row.get("pane_pid") or current.get("dead") != row.get("dead"):
            raise ReorganiseError(f"after verification found changed pane identity for {row['pane_id']}")
        if _identity_tuple(current.get("agent")) != _identity_tuple(row.get("agent")):
            raise ReorganiseError(f"after verification found changed agent identity for {row['pane_id']}")
    names_before = {row["window_id"]: row["window_name"] for row in snapshot.get("windows", [])}
    names_after = {row["window_id"]: row["window_name"] for row in after.get("windows", [])}
    if any(names_after.get(window_id) != name for window_id, name in names_before.items()):
        raise ReorganiseError("after verification found a changed original window name")
    panes_before = {row["window_id"]: set(row.get("pane_ids", [])) for row in snapshot.get("windows", [])}
    panes_after = {row["window_id"]: set(row.get("pane_ids", [])) for row in after.get("windows", [])}
    if any(panes_after.get(window_id) != panes for window_id, panes in panes_before.items()):
        raise ReorganiseError("after verification found a changed original window pane set")
    clients_after = {row.get("client_name"): row for row in after.get("clients", [])}
    for client in snapshot.get("clients", []):
        live = clients_after.get(client.get("client_name"))
        if live is None or live.get("pane_id") != client.get("pane_id"):
            raise ReorganiseError(
                f"after verification could not preserve client focus for {client.get('client_name')!r}"
            )


def _session_counts(snapshot: dict[str, Any]) -> dict[str, Any]:
    counts = [
        {"session": row["session_name"], "windows": row["window_count"]}
        for row in sorted(snapshot.get("sessions", []), key=lambda row: row["session_name"])
    ]
    return {"session_count": len(counts), "window_count": sum(row["windows"] for row in counts), "sessions": counts}


def _is_noop(snapshot: dict[str, Any], plan: dict[str, Any], tmux: Tmux) -> bool:
    wanted = {row["session"]: row["windows"] for row in plan["groups"]}
    if _current_groups(snapshot) != wanted:
        return False
    sid_by_name = _sid_by_name(snapshot)
    by_id = {row["window_id"]: row for row in snapshot.get("windows", [])}
    for group in plan["groups"]:
        sid = sid_by_name.get(group["session"])
        if sid is None:
            return False
        first = _base_index(tmux, sid)
        if [by_id[window_id]["window_index"] for window_id in group["windows"]] != list(
            range(first, first + len(group["windows"]))
        ):
            return False
    scratch = plan.get("scratchpad")
    if scratch is None or scratch in wanted:
        return True
    # A scratch-only session cannot be a no-op if it owns any original window:
    # those windows must be moved to one of the planned groups.
    return False


def _restore_names(mutator: Mutator, snapshot: dict[str, Any]) -> None:
    for row in snapshot.get("windows", []):
        current = _format(mutator.tmux, row["window_id"], "#{window_name}")
        if current != row["window_name"]:
            mutator.command(
                f"restore name of {row['window_id']}",
                "rename-window", "-t", row["window_id"], row["window_name"],
            )


def apply_plan(snapshot_path: str, plan_path: str) -> dict[str, Any]:
    snapshot = _read_json(snapshot_path)
    plan = _read_json(plan_path)
    if not isinstance(snapshot, dict) or snapshot.get("schema_version") != 1:
        raise ReorganiseError("unsupported snapshot schema; take a fresh snapshot")
    socket, server_pid, origin_index = _tmux_origin()
    server = snapshot.get("server") or {}
    if server.get("socket_path") != socket or server.get("server_pid") != server_pid:
        raise ReorganiseError("snapshot belongs to a different tmux server")
    _ensure_unlinked_ungrouped(snapshot)
    normalized_plan = _validate_plan(snapshot, plan)
    current = take_snapshot(0)
    _check_fresh(snapshot, current)
    _ensure_unlinked_ungrouped(current)
    destination_by_window = _window_to_group(normalized_plan)
    _focus_conflict(current, destination_by_window)
    state_root = Path(os.environ.get("XDG_STATE_HOME", str(Path.home() / ".local" / "state")))
    run_root = state_root / "tmux-agent-workbench" / "reorganise"
    _private_dir(run_root)
    run_dir = run_root / _run_id()
    _private_dir(run_dir)
    _write_private(run_dir / "before.json", current)
    _write_private(run_dir / "plan.json", normalized_plan)
    journal = Journal(run_dir)
    mutator = Mutator(Tmux(socket, server_pid), journal)
    original_session_by_name = _sid_by_name(current)
    session_ids: dict[str, str] = {}
    placeholders: list[dict[str, Any]] = []
    scratch_shell: dict[str, Any] | None = None
    manifest = {
        "schema_version": 1,
        "run_id": run_dir.name,
        "status": "running",
        "server": {"socket_path": socket, "server_pid": server_pid, "origin_session_index": origin_index},
        "placeholders": placeholders,
        "scratch_shell": None,
    }
    _write_run_manifest(run_dir, manifest)
    try:
        if not _is_noop(current, normalized_plan, mutator.tmux):
            # Prepare every destination before moving any existing window. This
            # keeps a destination alive when its own original windows move out.
            for group in normalized_plan["groups"]:
                name = group["session"]
                sid = original_session_by_name.get(name)
                if sid is None:
                    sid, placeholder_id = _new_session(mutator, name, _cwd_for_group(current, group["windows"]), label="group")
                    identity = _placeholder_identity(mutator.tmux, placeholder_id)
                else:
                    placeholder_id = _new_placeholder(
                        mutator, sid, name, _cwd_for_group(current, group["windows"]), label="group"
                    )
                    identity = _placeholder_identity(mutator.tmux, placeholder_id)
                session_ids[name] = sid
                identity.update({"session_id": sid, "session_name": name, "role": "group"})
                placeholders.append(identity)
                manifest["placeholders"] = placeholders
                _write_run_manifest(run_dir, manifest)
            scratch_name = normalized_plan.get("scratchpad")
            if scratch_name and scratch_name not in session_ids:
                sid = original_session_by_name.get(scratch_name)
                if sid is None:
                    sid, placeholder_id = _new_session(mutator, scratch_name, None, label="scratch")
                else:
                    placeholder_id = _new_placeholder(mutator, sid, scratch_name, None, label="scratch")
                identity = _placeholder_identity(mutator.tmux, placeholder_id)
                identity.update({"session_id": sid, "session_name": scratch_name, "role": "scratch"})
                placeholders.append(identity)
                scratch_shell = identity
                session_ids[scratch_name] = sid
                manifest["placeholders"] = placeholders
                manifest["scratch_shell"] = scratch_shell
                _write_run_manifest(run_dir, manifest)
            else:
                for group in normalized_plan["groups"]:
                    session_ids.setdefault(group["session"], original_session_by_name.get(group["session"], ""))

            _prepare_client_sessions(mutator, current.get("clients", []), destination_by_window, session_ids)

            source_session_for_window = {
                row["window_id"]: row["session_id"] for row in current.get("windows", [])
            }
            for group in normalized_plan["groups"]:
                destination_sid = session_ids[group["session"]]
                for window_id in group["windows"]:
                    if source_session_for_window[window_id] == destination_sid:
                        continue
                    mutator.command(
                        f"move {window_id} to session {group['session']}",
                        "move-window", "-s", window_id, "-t", f"{destination_sid}:",
                    )

            placeholder_by_session = {row["session_id"]: row for row in placeholders}
            for group in normalized_plan["groups"]:
                sid = session_ids[group["session"]]
                placeholder = placeholder_by_session.get(sid)
                if placeholder and placeholder.get("role") == "group":
                    _order_group(mutator, sid, group["windows"], placeholder["window_id"])
                else:
                    _order_group(mutator, sid, group["windows"], None)

            _restore_names(mutator, current)

            # A scratch-only placeholder is retained and renamed only after all
            # original windows have moved out of its session.
            for placeholder in list(placeholders):
                if placeholder.get("role") == "scratch":
                    wid = placeholder["window_id"]
                    mutator.command("name retained scratch shell", "rename-window", "-t", wid, "scratchpad")
                    mutator.command("lock retained scratch shell name", "set-window-option", "-t", wid, "automatic-rename", "off")
                    continue
                _safe_cleanup_placeholder(mutator, placeholder)

            _locate_clients(mutator, current.get("clients", []))
        else:
            session_ids = _sid_by_name(current)

        after = take_snapshot(0)
        _verify_apply(current, normalized_plan, after, scratch_shell)
        _write_private(run_dir / "after.json", after)
        manifest["status"] = "complete"
        manifest["completed_at"] = _datetime.datetime.now(_datetime.timezone.utc).isoformat()
        manifest["placeholders"] = placeholders
        manifest["scratch_shell"] = scratch_shell
        _write_run_manifest(run_dir, manifest)
        result = _session_counts(after)
        result.update({"run_dir": str(run_dir), "status": "complete"})
        return result
    except BaseException as exc:
        manifest["status"] = "failed"
        manifest["failure"] = str(exc)
        manifest["placeholders"] = placeholders
        manifest["scratch_shell"] = scratch_shell
        _write_run_manifest(run_dir, manifest)
        if isinstance(exc, ReorganiseError):
            raise ReorganiseError(f"{exc} (run journal: {run_dir})") from exc
        raise


def _restore_plan_from_snapshot(snapshot: dict[str, Any]) -> dict[str, Any]:
    grouped = _snapshot_expected_groups(snapshot)
    groups = [{"session": name, "windows": windows} for name, windows in sorted(grouped.items())]
    return {"groups": groups, "scratchpad": None}


def restore_run(run_dir_text: str) -> dict[str, Any]:
    run_dir = Path(run_dir_text).expanduser().resolve()
    if not run_dir.is_dir():
        raise ReorganiseError(f"run directory does not exist: {run_dir}")
    before = _read_json(run_dir / "before.json")
    after = _read_json(run_dir / "after.json")
    manifest = _read_json(run_dir / "run.json")
    if not isinstance(before, dict) or not isinstance(after, dict) or not isinstance(manifest, dict):
        raise ReorganiseError("run directory is incomplete or malformed")
    if manifest.get("status") != "complete":
        raise ReorganiseError("run is not complete; restore requires a verified after snapshot")
    socket, server_pid, origin_index = _tmux_origin()
    server = before.get("server") or {}
    if server.get("socket_path") != socket or server.get("server_pid") != server_pid:
        raise ReorganiseError("run belongs to a different tmux server")
    current = take_snapshot(0)
    _check_restore_fresh(after, current)
    _ensure_unlinked_ungrouped(current)
    restore_plan = _restore_plan_from_snapshot(before)
    _focus_conflict(current, _window_to_group(restore_plan))
    scratch = manifest.get("scratch_shell")
    scratch_identity = scratch if isinstance(scratch, dict) else None
    if scratch_identity:
        if not _placeholder_matches(Tmux(socket, server_pid), scratch_identity):
            raise ReorganiseError("task-owned scratch shell changed; refusing to remove or overwrite it")
        if any(client.get("window_id") == scratch_identity["window_id"] for client in current.get("clients", [])):
            raise ReorganiseError(
                "task-owned scratch shell is being viewed; move attached clients to an original window before restoring"
            )

    run_id = run_dir.name + "-restore-" + uuid.uuid4().hex[:8]
    restore_dir = run_dir / run_id
    _private_dir(restore_dir)
    _write_private(restore_dir / "before.json", current)
    _write_private(restore_dir / "plan.json", restore_plan)
    journal = Journal(restore_dir)
    mutator = Mutator(Tmux(socket, server_pid), journal)
    current_session_by_name = _sid_by_name(current)
    target_ids: dict[str, str] = {}
    placeholders: list[dict[str, Any]] = []
    try:
        # Prepare original sessions before removing a scratch-only window. In
        # the common case the scratch window is the last window in its session.
        for group in restore_plan["groups"]:
            name = group["session"]
            sid = current_session_by_name.get(name)
            if sid is None:
                sid, placeholder_id = _new_session(mutator, name, _cwd_for_group(before, group["windows"]), label="restore")
                identity = _placeholder_identity(mutator.tmux, placeholder_id)
            else:
                placeholder_id = _new_placeholder(
                    mutator, sid, name, _cwd_for_group(before, group["windows"]), label="restore"
                )
                identity = _placeholder_identity(mutator.tmux, placeholder_id)
            identity.update({"session_id": sid, "session_name": name, "role": "restore"})
            placeholders.append(identity)
            target_ids[name] = sid

        _prepare_client_sessions(mutator, current.get("clients", []), _window_to_group(restore_plan), target_ids)

        # Remove only the recorded scratch shell. Its full shell fingerprint
        # was verified above against both the run manifest and live tmux.
        if scratch_identity:
            _safe_cleanup_placeholder(mutator, scratch_identity)

        source_session = {row["window_id"]: row["session_id"] for row in current.get("windows", [])}
        for group in restore_plan["groups"]:
            destination_sid = target_ids[group["session"]]
            for window_id in group["windows"]:
                if source_session.get(window_id) == destination_sid:
                    continue
                mutator.command(
                    f"restore {window_id} to session {group['session']}",
                    "move-window", "-s", window_id, "-t", f"{destination_sid}:",
                )
        placeholders_by_sid = {row["session_id"]: row for row in placeholders}
        for group in restore_plan["groups"]:
            sid = target_ids[group["session"]]
            _order_group(mutator, sid, group["windows"], placeholders_by_sid[sid]["window_id"])
        _restore_names(mutator, before)
        for placeholder in placeholders:
            _safe_cleanup_placeholder(mutator, placeholder)
        _locate_clients(mutator, current.get("clients", []))
        restored = take_snapshot(0)
        if _current_groups(restored) != _snapshot_expected_groups(before):
            raise ReorganiseError("restore verification found unexpected window grouping or order")
        before_panes = {row["pane_id"]: row for row in before.get("panes", [])}
        restored_panes = {row["pane_id"]: row for row in restored.get("panes", [])}
        for pane_id, row in before_panes.items():
            current_pane = restored_panes.get(pane_id)
            if not current_pane or current_pane.get("pane_pid") != row.get("pane_pid"):
                raise ReorganiseError(f"restore verification found changed pane identity for {pane_id}")
            if _identity_tuple(current_pane.get("agent")) != _identity_tuple(row.get("agent")):
                raise ReorganiseError(f"restore verification found changed agent identity for {pane_id}")
        before_window_panes = {row["window_id"]: set(row.get("pane_ids", [])) for row in before.get("windows", [])}
        restored_window_panes = {row["window_id"]: set(row.get("pane_ids", [])) for row in restored.get("windows", [])}
        if any(restored_window_panes.get(window_id) != panes for window_id, panes in before_window_panes.items()):
            raise ReorganiseError("restore verification found a changed original window pane set")
        restored_clients = {row.get("client_name"): row for row in restored.get("clients", [])}
        for client in current.get("clients", []):
            live = restored_clients.get(client.get("client_name"))
            if live is None or live.get("pane_id") != client.get("pane_id"):
                raise ReorganiseError(f"restore verification could not preserve client focus for {client.get('client_name')!r}")
        _write_private(restore_dir / "after.json", restored)
        _write_private(restore_dir / "run.json", {
            "schema_version": 1,
            "run_id": run_id,
            "status": "complete",
            "server": {"socket_path": socket, "server_pid": server_pid, "origin_session_index": origin_index},
        })
        result = _session_counts(restored)
        result.update({"restore_dir": str(restore_dir), "status": "complete"})
        return result
    except BaseException as exc:
        if isinstance(exc, ReorganiseError):
            raise ReorganiseError(f"{exc} (restore journal: {restore_dir})") from exc
        raise


def batch_worker() -> dict[str, str]:
    try:
        result = subprocess.run(
            ["agent-team", "start", "codex", "--scene", "batch", "--team", "--team-dry-run"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            errors="replace",
        )
    except OSError as exc:
        raise ReorganiseError(f"cannot run read-only batch resolver 'agent-team': {exc}") from exc
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise ReorganiseError(f"batch settings resolver failed: {detail}")
    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        raise ReorganiseError("batch settings resolver did not return JSON") from exc
    config = payload.get("config") if isinstance(payload, dict) else None
    if not isinstance(config, dict):
        raise ReorganiseError("batch settings resolver output is missing config")
    if config.get("scene") != "batch":
        raise ReorganiseError("batch settings resolver config.scene is missing or is not 'batch'")
    model = config.get("worker_model")
    effort = config.get("worker_effort")
    if not isinstance(model, str) or not model.strip():
        raise ReorganiseError("batch settings resolver config.worker_model is missing or empty")
    if not isinstance(effort, str) or not effort.strip():
        raise ReorganiseError("batch settings resolver config.worker_effort is missing or empty")
    return {"scene": "batch", "model": model, "reasoning_effort": effort}


def _positive_context(value: str) -> int:
    try:
        parsed = int(value)
    except ValueError as exc:
        raise argparse.ArgumentTypeError("must be an integer between 0 and 20") from exc
    if parsed < 0 or parsed > 20:
        raise argparse.ArgumentTypeError("must be between 0 and 20")
    return parsed


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="tmux-agent-workbench reorganise", description="reorganise tmux windows safely")
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("batch-worker", help="resolve batch worker model and effort without launching it")
    snap = sub.add_parser("snapshot", help="emit the current tmux topology as JSON")
    snap.add_argument("--context-lines", type=_positive_context, default=0)
    apply = sub.add_parser("apply", help="apply a window grouping plan")
    apply.add_argument("--snapshot", required=True)
    apply.add_argument("--plan", required=True)
    restore = sub.add_parser("restore", help="restore the original grouping from a completed run")
    restore.add_argument("run_dir")
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = _parser()
    args = parser.parse_args(argv)
    try:
        if args.command == "batch-worker":
            result = batch_worker()
        elif args.command == "snapshot":
            result = take_snapshot(args.context_lines)
        elif args.command == "apply":
            result = apply_plan(args.snapshot, args.plan)
        elif args.command == "restore":
            result = restore_run(args.run_dir)
        else:
            parser.error("unknown command")
            return 2
    except ReorganiseError as exc:
        print(f"tmux-agent-workbench reorganise: {exc}", file=sys.stderr)
        return 1
    sys.stdout.write(json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
