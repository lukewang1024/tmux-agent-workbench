#!/usr/bin/env python
"""Quota protocol, caching, and menu regression checks without live credentials."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[1]


class UsageTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        self.env = dict(os.environ, XDG_STATE_HOME=str(self.root / 'state'),
                        PATH=str(self.bin) + ':' + str(REPO / 'bin') + ':' + os.environ['PATH'],
                        USAGE_TEST_ROOT=str(self.root), TZ='Asia/Shanghai')
        self.script('ccusage', '#!/bin/sh\nprintf \'{"daily":[]}\\n\'\n')
        self.script('tmux', '''#!/bin/sh
case $1 in
show-option) printf 'codex\\n' ;;
list-clients) printf '/dev/ttys001\\n' ;;
display-menu) printf '%s\\n' "$@" > "$USAGE_TEST_ROOT/menu" ;;
esac
''')
        self.script('codex', '''#!/usr/bin/env python3
import json,os,sys
from pathlib import Path
root=Path(os.environ['USAGE_TEST_ROOT'])
with (root/'calls').open('a') as f: f.write('call\\n')
for line in sys.stdin:
 d=json.loads(line)
 if d.get('method')=='initialize':
  print(json.dumps({'id':d['id'],'result':{}}),flush=True)
 elif d.get('method')=='account/rateLimits/read':
  if (root/'fail').exists(): print(json.dumps({'id':d['id'],'error':{'code':-1}}),flush=True)
  else:
   quota={'limitId':'codex','planType':'pro','primary':{'usedPercent':4,'windowDurationMins':10080,'resetsAt':2000000000}}
   spark={'limitId':'codex_bengalfox','primary':{'usedPercent':99,'windowDurationMins':300}}
   print(json.dumps({'id':d['id'],'result':{'rateLimitResetCredits':{'availableCount':3,'credits':[{'title':'Full reset','status':'available','expiresAt':2100000000},{'title':'Full reset','status':'available','expiresAt':2000000000},{'title':'Full reset','status':'redeemed','expiresAt':1900000000}]},'rateLimits':spark,'rateLimitsByLimitId':{'codex':quota,'codex_bengalfox':spark}}}),flush=True)
''')

    def tearDown(self):
        self.temp.cleanup()

    def script(self, name, contents):
        p = self.bin / name
        p.write_text(contents)
        p.chmod(0o755)

    def usage(self, *args):
        return subprocess.check_output([str(REPO / 'bin/workbench-agent-usage'), *args],
                                       env=self.env, text=True, timeout=30)

    def calls(self):
        return len((self.root / 'calls').read_text().splitlines())

    def test_account_cache_manual_refresh_failure_and_menu(self):
        self.usage('refresh', 'codex')
        self.assertIn('7d 96%L · reset 05-18 11:33', self.usage('render', 'codex'))
        self.assertIn('96%', self.usage('badge', '120'))
        self.assertEqual(self.calls(), 1)
        self.usage('refresh', 'codex', 'automatic')
        self.assertEqual(self.calls(), 1)
        cache = self.root / 'state/tmux-agent-workbench/usage/codex'
        # Nine minutes: no automatic request; ten minutes: refresh eligible.
        import time
        os.utime(cache, (time.time()-540, time.time()-540))
        self.usage('refresh', 'codex', 'automatic')
        self.assertEqual(self.calls(), 1)
        os.utime(cache, (time.time()-601, time.time()-601))
        self.usage('refresh', 'codex', 'automatic')
        self.assertEqual(self.calls(), 2)
        self.usage('refresh-menu', '/dev/ttys001')
        self.assertEqual(self.calls(), 3)
        menu = (self.root / 'menu').read_text()
        self.assertIn('Refresh now\nr\n', menu)
        self.assertIn('pro 7d 96%L#[align=right]↻ 05-18 11:33', menu)
        rows = [line.split('},-,}', 1)[1] for line in menu.splitlines()
                if line.startswith('#{?#{==:1,1},-,}')]
        self.assertTrue(all(row.count("#[align=right]") == 1 for row in rows), rows)
        self.assertIn("Daily#[align=right]0 tokens", rows)
        self.assertIn("Cards ×3#[align=right]exp 05-18 11:33", rows)
        self.assertEqual(len(rows), 5)
        self.assertIn('workbench-agent-usage refresh-menu /dev/ttys001', menu)
        (self.root / 'fail').touch()
        self.usage('refresh', 'codex')
        self.assertIn('96%L · reset 05-18 11:33 (stale)', self.usage('render', 'codex'))
        self.usage('refresh', 'codex', 'automatic')
        self.assertEqual(self.calls(), 4)

    def test_unknown_plan_and_card_expiry_states(self):
        self.usage('refresh', 'codex')
        state = self.root / 'state/tmux-agent-workbench/usage'
        (state / 'codex').write_text('Cdx D0 W0 M0 | L7d 96%L · reset 05-18 11:33\n')
        for summary, expected in (
            ({'availableCount': 0, 'credits': []}, 'Cards ×0#[align=right]—'),
            ({'availableCount': 3, 'credits': None}, 'Cards ×3#[align=right]expiry unknown'),
            ({'availableCount': 1, 'credits': [{'status': 'available', 'expiresAt': None}]},
             'Cards ×1#[align=right]no expiry'),
            (None, None),
        ):
            (state / 'codex-limits.json').write_text(json.dumps({'payload': {'reset_credits': summary}}))
            self.usage('menu', '/dev/ttys001')
            menu = (self.root / 'menu').read_text()
            self.assertIn('7d 96%L#[align=right]↻ 05-18 11:33', menu)
            if expected:
                self.assertIn(expected, menu)
            else:
                self.assertNotIn('Cards ×', menu)

    def test_missing_quota_and_shared_lock(self):
        (self.root / 'fail').touch()
        self.usage('refresh', 'codex')
        self.assertIn('— (stale)', self.usage('render', 'codex'))
        lock = self.root / 'state/tmux-agent-workbench/usage/codex.refresh-lock'
        lock.mkdir()
        self.usage('refresh', 'codex')
        self.assertEqual(self.calls(), 1)
        self.assertEqual(self.usage('badge', '120'), '')


if __name__ == '__main__':
    unittest.main()
