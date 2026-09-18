#!/usr/bin/env python3
"""Run identical behavioral fault cases against a commit and the Hook-only patch.

No live daemon, hook configuration, or tmux session is changed. Outputs include
source snapshots, exact patch/hash, build logs, per-trial logs, JSON and Markdown.
"""
import argparse
import datetime
import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import tarfile

HOOK_FILES = [
    "bin/tmux-agent-workbench-cli", "src/checkpoint.rs", "src/daemon.rs",
    "src/detection.rs", "src/hooks.rs", "src/model.rs", "src/process.rs",
    "src/state_machine.rs",
]
CASES = {
    "screen_checkpoint": ("故障", "无 Hook 的检查点恢复", "保留新鲜 working/screen，不冒充 Hook 状态"),
    "legacy_unknown": ("故障", "旧 unknown 检查点恢复", "恢复 idle/screen，保留 session 绑定"),
    "late_checkpoint": ("故障", "新事件先到、旧检查点后到", "已收到的 working 不被旧 idle 覆盖"),
    "identical_payloads": ("故障", "两次内容相同的 Stop", "生成不同事件 ID，避免误去重"),
    "ambiguous_cwd": ("故障", "两个 CLI 使用同一目录", "拒绝歧义，不向任何窗格错误绑定"),
    "cwd_alias": ("故障", "真实路径和符号链接路径", "将事件正确关联到唯一窗格"),
    "delayed_ack": ("故障", "Socket 确认延迟 1100ms", "不报丢失，原事件 ID 留在重试队列"),
    "explicit_duplicate_control": ("对照", "同一显式 ID 重复投递", "保留同一个提醒 ID，不重复产生提醒"),
    "lifecycle_control": ("对照", "正常主回合生命周期", "working → blocked → idle"),
    "thread_fence_control": ("对照", "后台线程发 Stop", "拒绝事件，前台继续 working"),
}


def command(args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", default="95cda17")
    parser.add_argument("--repo-dir", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--patch", type=Path, help="Replay an exact saved candidate patch")
    parser.add_argument("--fixtures", type=Path, help="Replay exact saved Rust fixtures")
    parser.add_argument("--repeats", type=int, default=10)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if args.repeats < 1:
        parser.error("--repeats must be positive")
    repo = args.repo_dir.resolve()
    baseline = subprocess.check_output(["git", "rev-parse", args.baseline], cwd=repo, text=True).strip()
    timestamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    state = Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local/state"))
    output = (args.output or state / "tmux-agent-workbench/hook-comparison" / timestamp).resolve()
    output.mkdir(parents=True, exist_ok=False)
    cache = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache"))
    env = dict(os.environ, CARGO_TARGET_DIR=str(cache / "tmux-agent-workbench/hook-comparison-target"), CARGO_NET_OFFLINE="true")
    archive = subprocess.check_output(["git", "archive", baseline], cwd=repo)
    patch = args.patch.read_bytes() if args.patch else subprocess.check_output(["git", "diff", baseline, "--", *HOOK_FILES], cwd=repo)
    (output / "candidate.patch").write_bytes(patch)
    fixture_dir = args.fixtures or repo / "tests/hook-comparison"
    fixtures = {name: (fixture_dir / f"{name}.rs").read_bytes()
                for name in ("state_machine", "hooks", "detection")}
    (output / "runner.py").write_bytes(Path(__file__).read_bytes())
    (output / "harness").mkdir()
    for name, content in fixtures.items():
        (output / f"harness/{name}.rs").write_bytes(content)
    results = {}
    for version in ("before", "after"):
        tree = output / version
        tree.mkdir()
        with tarfile.open(fileobj=io.BytesIO(archive)) as source:
            source.extractall(tree, filter="data")
        if version == "after" and patch:
            command(["git", "apply", "-"], cwd=tree, input=patch)
        for name, content in fixtures.items():
            (tree / f"src/hook_comparison_{name}.rs").write_bytes(content)
            with (tree / f"src/{name}.rs").open("a") as target:
                target.write(f'\n#[cfg(test)]\n#[path = "hook_comparison_{name}.rs"]\nmod hook_comparison;\n')
        print(f"{version}: compiling {baseline[:8]} {'+ Hook patch' if version == 'after' else ''}", flush=True)
        build = subprocess.run(["cargo", "test", "--locked", "--lib", "--no-run", "--message-format=json"],
                               cwd=tree, env=env, text=True, capture_output=True)
        (output / f"{version}-build.log").write_text(build.stdout + "\n" + build.stderr)
        if build.returncode:
            raise RuntimeError(f"{version} compilation failed; inspect {output}/{version}-build.log")
        artifacts = [json.loads(line) for line in build.stdout.splitlines() if line.startswith("{")]
        binaries = [a["executable"] for a in artifacts if a.get("reason") == "compiler-artifact"
                    and a.get("executable") and a.get("profile", {}).get("test")]
        if len(binaries) != 1:
            raise RuntimeError(f"Expected one library test executable, found {binaries}")
        trials = []
        for trial in range(1, args.repeats + 1):
            run = subprocess.run([binaries[0], "hook_comparison::", "--test-threads=1", "--nocapture"],
                                 cwd=tree, env=env, text=True, capture_output=True, timeout=60)
            raw = run.stdout + "\n" + run.stderr
            (output / f"{version}-trial-{trial:02d}.log").write_text(raw)
            rows = [json.loads(m.group(1)) for m in re.finditer(r"HOOK_CASE_RESULT (\{[^\n]+\})", raw)]
            if len(rows) != len(CASES) or {r["case"] for r in rows} != set(CASES):
                raise RuntimeError(f"Incomplete trial {version}/{trial}; inspect raw log")
            if run.returncode not in (0, 101) or (run.returncode == 0) != all(r["passed"] for r in rows):
                raise RuntimeError(f"Unexpected test process outcome: {version}/{trial}")
            trials.append({"trial": trial, "exit_code": run.returncode, "cases": rows})
            print(f"{version}: trial {trial}/{args.repeats}: {sum(r['passed'] for r in rows)}/{len(rows)} passed", flush=True)
        results[version] = trials
    report = {
        "created_utc": timestamp, "baseline": baseline,
        "candidate_definition": "baseline + exact candidate.patch (Hook files only)",
        "patch_sha256": hashlib.sha256(patch).hexdigest(),
        "fixture_sha256": {name: hashlib.sha256(content).hexdigest() for name, content in fixtures.items()},
        "runner_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "repeats": args.repeats, "cases": CASES, "results": results,
        "limits": "Targeted deterministic regression suite, not a production success-rate estimate. "
                  "Delayed-ACK test verifies safe queuing, not full replay delivery. "
                  "Does not exercise full daemon replacement, shared backend topology, or persisted replay guards.",
    }
    (output / "results.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    lines = ["# Hook 修复前后对比", "", f"基线：`{baseline}`；修复后：基线叠加保存的 `candidate.patch`。",
             f"每个场景运行 {args.repeats} 次；两边使用完全相同的输入、测试源码和判定标准。", "",
             "| 场景 | 类型 | 通过标准 | 修复前 | 修复后 |", "|---|---|---|---:|---:|"]
    for case, (kind, name, expected) in CASES.items():
        counts = [sum(r["passed"] for t in results[v] for r in t["cases"] if r["case"] == case)
                  for v in ("before", "after")]
        lines.append(f"| {name} | {kind} | {expected} | {counts[0]}/{args.repeats} | {counts[1]}/{args.repeats} |")
    lines += ["", "## 汇总", ""]
    for version in ("before", "after"):
        rows = [r for t in results[version] for r in t["cases"]]
        passed = sum(r["passed"] for r in rows)
        lines.append(f"- {version}: {passed}/{len(rows)} = {passed/len(rows):.0%}")
    lines += ["", "## 首次运行的实际观测", "", "```json",
              json.dumps({v: results[v][0]["cases"] for v in results}, ensure_ascii=False, indent=2), "```", "",
              "## 适用边界", "", "这是针对已知故障构造的回归样本，通过率不是线上所有 session 的成功率。",
              "重复运行用于观察稳定性，不代表独立随机样本，也不用于推断统计置信区间。",
              "延迟确认场景只证明原事件进入重试队列；本组未覆盖完整守护进程替换、共享后端拓扑和持久化重放保护。",
              "完整输入、补丁、源码快照、构建日志和每次执行日志均保留在本报告目录。", "",
              f"补丁 SHA-256：`{report['patch_sha256']}`", ""]
    import shlex
    replay = ["python3", str(output / "runner.py"), "--repo-dir", str(repo), "--baseline", baseline,
              "--patch", str(output / "candidate.patch"), "--fixtures", str(output / "harness"),
              "--repeats", str(args.repeats)]
    lines += ["## 原样复跑", "", "```sh", shlex.join(replay), "```", ""]
    (output / "report.md").write_text("\n".join(lines))
    print(f"Report: {output / 'report.md'}", flush=True)


if __name__ == "__main__":
    main()
