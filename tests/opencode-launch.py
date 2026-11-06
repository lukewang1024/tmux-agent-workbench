"""Verify private-server selection without launching an agent."""
import json
import os
from pathlib import Path
import subprocess
import tempfile

repo = Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix='wb-opencode-launch-') as directory:
    root = Path(directory)
    opencode = root / 'opencode'
    opencode.write_text('#!/bin/sh\nprintf "%s\\n" "$WB_TEST_VERSION"\n')
    opencode.chmod(0o755)
    team = root / 'agent-team'
    team.write_text('#!/usr/bin/env python3\nimport json,sys\nprint(json.dumps(sys.argv[1:]))\n')
    team.chmod(0o755)
    env = dict(os.environ, PATH=str(root) + os.pathsep + os.environ['PATH'], WORKBENCH_MANAGED='0')
    for key in ('TMUX', 'TMUX_PANE', 'AGENT_TEAM_DIR'):
        env.pop(key, None)

    def launch(version, *args):
        result = subprocess.check_output([str(repo/'bin/wb-agent'), 'opencode', *args],
                                         env=dict(env, WB_TEST_VERSION=version), text=True)
        return json.loads(result)

    assert launch('opencode v2.0.25') == ['start', 'opencode', '--standalone', '--auto']
    assert launch('opencode v2.0.25', '--auto') == ['start', 'opencode', '--standalone', '--auto']
    assert launch('opencode v2.0.25', '--server', 'http://local') == ['start', 'opencode', '--auto', '--server', 'http://local']
    assert launch('opencode v2.0.25', '--team') == ['start', 'opencode', '--team']
    assert launch('1.18.32') == ['start', 'opencode', '--auto']
    print('PASS OpenCode launcher: private v2 service and explicit server/team preferences')
