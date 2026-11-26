import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { once } from 'node:events';
import { appendFile, mkdir, mkdtemp, open, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { WebSocket } from 'ws';
import { Auth } from '../auth.mjs';
import { codexComposer, codexFullAccessConfirmation, codexSettings, modelChangeConfirmed, permissionChoices } from '../codex.mjs';
import { enrollNode, runConnector } from '../connector.mjs';
import { History } from '../history.mjs';
import { resolveLanguage } from '../i18n.mjs';
import { ChatState } from '../public/chat-state.js';
import { startServer } from '../server.mjs';
import { hash, readJson, Store } from '../state.mjs';
import { tmuxPalette } from '../theme.mjs';
import { LocalNode } from '../tmux.mjs';

async function scratch(t) {
  const directory = await mkdtemp(join(tmpdir(), 'wb-web-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  return directory;
}

test('Codex permission choices use distinct reviewers and respect host constraints', () => {
  const profiles = [':read-only', ':workspace', ':danger-full-access'].map((id) => ({ id }));
  assert.deepEqual(permissionChoices(profiles, null).map(({ mode, arguments: args }) => [mode, args]), [
    ['askForApproval', ':workspace --approval-policy=on-request --reviewer=user'],
    ['approveForMe', ':workspace --approval-policy=on-request --reviewer=auto_review'],
    ['yolo', ':danger-full-access --approval-policy=never --reviewer=user'],
  ]);
  assert.deepEqual(permissionChoices(profiles, { allowedApprovalsReviewers: ['user'], allowedApprovalPolicies: ['on-request'] })
    .map(({ mode }) => mode), ['askForApproval']);
  const confirmation = '› 1. Yes, continue anyway  Apply full access for this session\n'
    + '  2. Cancel                Go back without enabling full access\n\n  enter select · esc back';
  assert.equal(codexFullAccessConfirmation(confirmation), true);
  assert.equal(codexFullAccessConfirmation(confirmation.replace('› 1.', '  1.')), false);
});

test('tmux palette preserves indexed and true colors while discarding invalid roles', () => {
  const line = 'dark|full|||' + JSON.stringify({
    version: 1, background: '#282c34', accent: 'colour0', warning: 'color231',
    alert: 'bright-red', info: 'colour256', muted: 'var(--unexpected)', extra: '#ffffff',
  });
  assert.deepEqual(tmuxPalette(line), {
    appearance: 'dark', full: true, roles: { background: '#282c34', accent: 0, warning: 231, alert: 9 },
  });
  assert.deepEqual(tmuxPalette('light|full|colour5|bg=#fafafa,fg=#383a42,none|'), {
    appearance: 'light', full: true, roles: { background: '#fafafa', foreground: '#383a42', accent: 5 },
  });
  assert.equal(tmuxPalette('light|light|||{"version":2}'), undefined);
  assert.equal(tmuxPalette('light|light|||broken'), undefined);
});

test('Codex settings read the live footer and distinguish the placeholder from terminal drafts', () => {
  const models = [{ id: 'gpt-6.2-sol', label: 'GPT-6.2-Sol' }, { id: 'gpt-6.1-terra', label: 'GPT-6.1 Terra' }];
  const footer = '  GPT-6.1 Terra max · fixture';
  const placeholder = '\x1b[1m›\x1b[0m \x1b[2mAsk Codex to do anything\x1b[0m';
  const screen = ['  GPT-6.2-Sol high · earlier command', placeholder, footer].join('\n');
  assert.deepEqual(codexSettings(screen, 1, models), { model: 'gpt-6.1-terra', effort: 'max' });
  assert.deepEqual(codexSettings(placeholder + '\n  GPT-6.2-Sol default · fixture', 0, models),
    { model: 'gpt-6.2-sol', effort: 'default' });
  assert.deepEqual(codexSettings(placeholder + '\n  repo · GPT-6.2-Sol high · fixture', 0, models),
    { model: 'gpt-6.2-sol', effort: 'high' });
  assert.equal(codexComposer(screen, 1), '');
  assert.equal(codexComposer('› Ask Codex to do anything\n' + footer, 0), 'Ask Codex to do anything');
  assert.equal(codexComposer('› A native draft\n  continued\n' + footer, 1), 'A native draft continued');
  assert.equal(modelChangeConfirmed('• Model changed to gpt-6.2-sol ultra for this session only', 'gpt-6.2-sol', 'ultra'), 1);
  assert.equal(modelChangeConfirmed('• Model changed to gpt-6.2-sol high for this session only', 'gpt-6.2-sol', 'ultra'), 0);
});

test('current Codex settings follow authenticated thread events without projecting file paths or instructions', async (t) => {
  const directory = await scratch(t);
  const id = 'c5a27de3-936f-446e-9de1-797eaa1cfc00';
  const sessions = join(directory, 'sessions');
  await mkdir(sessions);
  const file = join(sessions, 'rollout-fixture-' + id + '.jsonl');
  const rows = [{ type: 'session_meta', payload: { id } }, {
    type: 'turn_context', payload: {
      model: 'gpt-6.2-sol', effort: 'medium', sandbox_policy: { type: 'danger-full-access' },
      approval_policy: 'never', approvals_reviewer: 'user',
    },
  }];
  const write = () => writeFile(file, rows.map((row) => JSON.stringify(row)).join('\n') + '\n');
  const history = new History(directory);
  await write();
  assert.deepEqual(await history.latestSettings(id),
    { model: 'gpt-6.2-sol', effort: 'medium', permission: { mode: 'yolo', id: null } });
  rows.push({
    type: 'event_msg', payload: { type: 'thread_settings_applied', thread_id: id,
      thread_settings: { model: 'gpt-6.2-astra', reasoning_effort: 'high', approval_policy: 'on-request',
        approvals_reviewer: 'auto_review', active_permission_profile: { id: ':workspace' },
        collaboration_mode: { settings: { developer_instructions: 'Fixture instructions' } },
        cwd: '/workspace/example', runtime_workspace_roots: ['/workspace/example'] },
    },
  });
  await write();
  assert.deepEqual(await history.latestSettings(id),
    { model: 'gpt-6.2-astra', effort: 'high', permission: { mode: 'approveForMe', id: ':workspace' } });
});

test('node invites are scoped to one identity, single-use, and revocable', async (t) => {
  const store = new Store(await scratch(t));
  const invitation = store.issue('node', 'example-node');
  assert.deepEqual([...store.pendingNodes()], ['example-node']);
  assert.throws(() => store.invitation(invitation, 'passkey'), { status: 403 });
  const enrolled = store.enroll(invitation);
  assert.equal(enrolled.id, 'example-node');
  assert.equal(store.enrolled(enrolled.id).digest, hash(enrolled.credential));
  assert.equal(store.pendingNodes().size, 0);
  assert.throws(() => store.enroll(invitation), { status: 403 });
  assert.throws(() => store.enrolled('../example'), { status: 400 });
  assert.equal(store.revokeNode(enrolled.id), true);
  assert.equal(store.enrolled(enrolled.id), null);
});

test('device links carry a passkey invitation for the configured origin and can only be used once', async (t) => {
  const store = new Store(await scratch(t));
  const origin = 'http://localhost:7080';
  const started = Date.now();
  const { url, expiresInMinutes } = store.issuePasskeyLink(origin);
  const link = new URL(url);
  const invite = new URLSearchParams(link.hash.slice(1)).get('invite');
  assert.equal(link.origin, origin);
  assert.equal(link.pathname, '/');
  assert.equal(expiresInMinutes, 10);
  assert.throws(() => store.invitation(invite, 'node'), { status: 403 });
  const { expires } = store.invitation(invite, 'passkey', true);
  assert.ok(expires >= started + expiresInMinutes * 60_000);
  assert.ok(expires <= Date.now() + expiresInMinutes * 60_000);
  assert.throws(() => store.invitation(invite, 'passkey', true), { status: 403 });
});

test('browser sessions survive reloads with the same origin and respect expiry and revocation', async (t) => {
  const store = new Store(await scratch(t));
  const origin = new URL('http://localhost:7080');
  store.writeKeys([{ id: 'primary' }, { id: 'backup' }]);
  const auth = new Auth(store, origin);
  const cookie = auth.issueSession('primary').split(';')[0];
  const request = { headers: { cookie } };
  const key = auth.session(request, false);
  assert.ok(key);
  assert.equal(key, hash(cookie.split('=')[1]));
  assert.ok(!JSON.stringify(store.sessions(origin.origin)).includes(cookie.split('=')[1]));
  assert.equal(new Auth(store, origin).session(request, false), key);
  assert.equal(new Auth(store, new URL('http://localhost:7081')).session(request, false), null);
  auth.revoke('primary');
  assert.equal(new Auth(store, origin).session(request, false), null);
  const other = auth.issueSession('backup').split(';')[0];
  const otherRequest = { headers: { cookie: other } };
  const otherKey = auth.session(otherRequest, false);
  auth.sessions.get(otherKey).idle = Date.now() - 1;
  auth.persistSessions();
  assert.equal(new Auth(store, origin).session(otherRequest, false), null);
});

test('chat state restores a pending delivery and reconciles it after the pane acquires a thread', () => {
  const storage = {
    getItem(key) { return this[key]; },
    setItem(key, value) { this[key] = value; },
    removeItem(key) { delete this[key]; },
  };
  const initial = new ChatState(() => storage);
  const pane = 'local/agent-1/1.2.3.4';
  const thread = 'local/thread/c5a27de3-936f-446e-9de1-797eaa1cfc00';
  const text = 'Original message';
  const first = initial.beginDelivery(pane, { text, after: null, replaceDraft: true }).entry;
  initial.queueSettings(pane, { model: 'gpt-6.2-sol', effort: 'high', permission: ':read-only' });
  const restored = new ChatState(() => storage);
  restored.restore('Unconfirmed');
  assert.equal(restored.draft(pane), text);
  const settings = restored.settings(pane).pending;
  restored.promote(pane, thread);
  assert.equal(restored.settings(thread).pending, settings);
  assert.equal(restored.draft(pane), '');
  const previous = restored.failedDelivery(thread, text);
  assert.equal(previous.id, first.id);
  assert.equal(previous.error, 'Unconfirmed');
  restored.saveDraft(thread, 'A newer draft');
  const retry = restored.beginDelivery(thread, { text, previous, after: 12, replaceDraft: false });
  assert.equal(retry.entry.id, first.id);
  assert.equal(retry.clearDraft, false);
  const before = restored.reconcile(thread, [{ id: 11, role: 'user', text }]);
  assert.equal(before.changed, false);
  restored.sent(retry.entry, { after: 12 });
  assert.deepEqual(restored.reconcile(thread, [{ id: 12, role: 'user', text }]), { changed: true, draftCleared: false });
  assert.deepEqual(restored.deliveries(thread), []);
  assert.equal(restored.draft(thread), 'A newer draft');
  restored.applySettings(thread, { model: 'gpt-6.2-sol', effort: 'high' });
  assert.deepEqual(restored.settings(thread).pending, { permission: ':read-only' });
});

test('Codex history is tied to an exact rollout and only returns visible text', async (t) => {
  const directory = await scratch(t);
  const session = 'c5a27de3-936f-446e-9de1-797eaa1cfc00';
  const target = join(directory, 'sessions', '2026', '01', '01');
  await mkdir(target, { recursive: true });
  const message = (text, role) => ({ type: 'response_item', payload: { type: 'message', role, content: [{ type: role === 'user' ? 'input_text' : 'output_text', text }] } });
  const goal = '<codex_internal_context source="goal">\nContinue the active goal.\n</codex_internal_context>';
  const userGoal = '<codex_internal_context source="user_goal">\nUser set the goal.\n</codex_internal_context>';
  const rows = [{ type: 'session_meta', payload: { id: session } }];
  rows.push(message('system instructions', 'developer'), message('<environment_context>private</environment_context>', 'user'),
    message(userGoal + '\n\n' + goal, 'user'));
  for (let index = 0; index < 50; index++) {
    const text = index === 0 ? userGoal + '\n\n' + goal + '\n\nmessage 0'
      : index === 2 ? '    a code block' : index === 4 ? 'Quoted example:\n' + goal : 'message ' + index;
    rows.push(message(text, index % 2 ? 'assistant' : 'user'));
  }
  const rollout = rows.map((row) => JSON.stringify(row)).join('\n') + '\n{"partial"';
  await writeFile(join(target, 'rollout-date-' + session + '.jsonl'), rollout);
  const history = new History(directory);
  assert.equal(await history.endOffset(session), Buffer.byteLength(rollout));
  const latest = await history.page(session);
  assert.equal(latest.messages.length, 35);
  assert.equal(latest.messages.at(-1).text, 'message 49');
  assert.ok(latest.before);
  const earlier = await history.page(session, latest.before);
  assert.equal(earlier.messages.length, 15);
  assert.equal(earlier.messages.at(0).text, 'message 0');
  assert.equal(earlier.messages.at(2).text, '    a code block');
  assert.equal(earlier.messages.at(4).text, 'Quoted example:\n' + goal);
  assert.equal(earlier.before, null);
  await assert.rejects(history.page('../wrong'), { status: 409 });
});

test('Codex history groups tool metadata by message and counts unfinished calls in the current turn', async (t) => {
  const directory = await scratch(t);
  const id = 'a3f18f9d-b84f-4362-b2d1-e0c06bfa2d0d';
  const sessionDirectory = join(directory, 'sessions');
  await mkdir(sessionDirectory);
  const filename = join(sessionDirectory, 'rollout-fixture-' + id + '.jsonl');
  const item = (payload, timestamp) => ({ type: 'response_item', payload, timestamp });
  const event = (type) => ({ type: 'event_msg', payload: { type } });
  const message = (role, text, phase) => item({ type: 'message', role, phase,
    content: [{ type: role === 'user' ? 'input_text' : 'output_text', text }] });
  const call = (name, call_id, timestamp, type = 'function_call') => item({
    type, name, call_id, arguments: 'private arguments', input: 'private input',
  }, timestamp);
  const output = (call_id, timestamp, type = 'function_call_output') => item({ type, call_id, output: 'private output' }, timestamp);
  const encode = (rows) => rows.map((row) => JSON.stringify(row) + '\n').join('');
  await writeFile(filename, encode([
    { type: 'session_meta', payload: { id } },
    event('task_started'), message('user', 'Old turn'), message('assistant', 'Old status', 'commentary'),
    call('old_tool', 'old'), event('turn_aborted'),
    event('task_started'), message('user', 'Current turn'),
    call('search', 'first', '2026-01-01T00:00:01Z', 'custom_tool_call'),
    output('first', '2026-01-01T00:00:02Z', 'custom_tool_call_output'),
    message('assistant', 'Checking changes', 'commentary'), call('read', 'second'), call('inspect', 'third'),
  ]));
  const history = new History(directory);
  let page = await history.page(id);
  assert.equal(page.runningTools, 2);
  assert.deepEqual(page.messages[1].tools, [{ name: 'old_tool', state: 'unconfirmed' }]);
  assert.deepEqual(page.messages[3].tools, [
    { name: 'search', state: 'finished', elapsed: 1000 }, { name: 'read', state: 'running' }, { name: 'inspect', state: 'running' },
  ]);
  await appendFile(filename, encode([output('third'), output('second'), message('assistant', 'Done', 'final_answer'), event('task_complete')]));
  page = await history.page(id);
  assert.equal(page.runningTools, undefined);
  assert.equal(page.messages[3].tools.every(({ state }) => state === 'finished'), true);
  assert.equal(page.messages[4].tools, undefined);
  assert.doesNotMatch(JSON.stringify(page), /private|call_id/);
});

test('Codex without hooks uses the rollout open in the verified live process', {
  skip: !['darwin', 'linux'].includes(process.platform),
}, async (t) => {
  const directory = await scratch(t);
  const sessions = join(directory, 'sessions');
  await mkdir(sessions);
  const primary = 'c5a27de3-936f-446e-9de1-797eaa1cfc00';
  const sibling = 'd5a27de3-936f-446e-9de1-797eaa1cfc00';
  const createRollout = async (id, metadata = id, folder = sessions) => {
    const filename = join(folder, 'rollout-date-' + id + '.jsonl');
    const record = JSON.stringify({ type: 'session_meta', payload: { id: metadata } });
    await writeFile(filename, record + '\n');
    return open(filename, 'r');
  };
  const current = await createRollout(primary);
  const unrelated = await createRollout(sibling, sibling, directory);
  const malformed = await createRollout(sibling, primary);
  const started = execFileSync('/bin/ps', ['-o', 'lstart=', '-p', String(process.pid)], {
    env: { ...process.env, LC_ALL: 'C', TZ: 'UTC' }, encoding: 'utf8',
  });
  const time = Date.parse(started.trim() + ' UTC') / 1000;
  const agent = { instance_id: 'codex', process: { pid: process.pid, started_at_ticks: time } };
  const history = new History(directory);
  const identify = (target = agent) => history.activeThreads([target]).then((result) => result.get('codex'));
  try {
    assert.equal(await identify(), primary);
    assert.equal(await identify({ ...agent, process: { ...agent.process, started_at_ticks: time + 1 } }), undefined);
    assert.equal(await identify({ ...agent, hook_session_id: sibling.slice(0, 30) }), undefined);
    assert.deepEqual(await history.page(primary), { messages: [], before: null });
    await malformed.close();
    const competing = await createRollout(sibling);
    try {
      assert.equal(await identify(), undefined);
      assert.equal(await identify({ ...agent, hook_session_id: primary.slice(0, 30) }), primary);
    } finally { await competing.close(); }
  } finally {
    await current.close();
    await unrelated.close();
    await malformed.close();
  }
});

test('tmux control mode streams a background Pane and sends input to that exact PTY', async (t) => {
  const directory = await scratch(t);
  const socket = join(directory, 't');
  // The interactive fixture must interpret the UTF-8 bytes used in the assertion.
  const environment = { ...process.env, LC_ALL: process.platform === 'darwin' ? 'en_US.UTF-8' : 'C.UTF-8' };
  delete environment.TMUX;
  delete environment.TMUX_PANE;
  const tmux = (...args) => execFileSync('tmux', ['-S', socket, ...args], { env: environment, encoding: 'utf8', timeout: 4000 }).trim();
  tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'fixture', 'exec /bin/sh -i');
  const original = tmux('display-message', '-p', '-t', 'fixture:', '#{pane_id}');
  const remote = tmux('new-window', '-d', '-P', '-F', '#{pane_id}', '-t', 'fixture:', 'exec /bin/sh -i');
  tmux('set-option', '-p', '-t', remote, '@workbench-web-pane', 'split');
  const node = new LocalNode({ socket, core: '/bin/false' });
  try {
    const pane = (await node.catalog()).terminals.find((entry) => entry.pane.pane === remote).pane;
    const opened = await node.watchOpen(pane.key);
    assert.equal(opened.resizable, false);
    await node.watchInput(opened.watch, "printf 'PTS:%s\\n' 验证\r");
    let output = Buffer.alloc(0);
    let cursor = opened.seq;
    for (let tries = 0; tries < 80 && !output.includes('\nPTS:验证'); tries++) {
      const read = await node.watchRead(opened.watch, cursor);
      cursor = read.seq;
      if (read.data) output = Buffer.concat([output, Buffer.from(read.data, 'base64')]);
      await delay(25);
    }
    assert.ok(output.includes('\nPTS:验证'), 'expected genuine UTF-8 terminal output');
    assert.equal(tmux('display-message', '-p', '-t', 'fixture:', '#{pane_id}'), original);
    assert.ok(!(await node.catalog()).terminals.some((entry) => entry.pane.pane === original));
    assert.equal((await node.create(null)).resizable, true);
    node.watchClose(opened.watch);
    await assert.rejects(node.watchInput(opened.watch, 'pwd\r'), { status: 410 });
  } finally {
    node.close();
    tmux('kill-server');
  }
});

test('a working Codex thread accepts a follow-up in the same Pane', async (t) => {
  const socket = join(await scratch(t), 't');
  const tmux = (...args) => execFileSync('tmux', ['-S', socket, ...args], { encoding: 'utf8', timeout: 4000 }).trim();
  const pane = tmux('-f', '/dev/null', 'new-session', '-ds', 'fixture', '-PF', '#{pane_id}', 'exec /bin/sh -i');
  const session = tmux('display-message', '-p', '-t', pane, '#{session_id}');
  const thread = 'c5a27de3-936f-446e-9de1-797eaa1cfc00';
  let state = 'working';
  const node = new LocalNode({ socket });
  node.history = new History(await scratch(t));
  node.snapshot = async () => ({
    sessions: [],
    agents: [{
      instance_id: 'agent', kind: 'codex', hook_session_id: thread, display_state: state,
      target: { pane_id: pane, session_id: session },
    }],
  });
  try {
    const [entry] = (await node.catalog()).agents;
    const receipt = await node.sendPrompt('agent', thread, "printf 'WORKING:%s\\n' received", entry.pane.key);
    assert.deepEqual(receipt, { after: 0 });
    for (let tries = 0; tries < 20 && !tmux('capture-pane', '-pt', pane).split('\n').includes('WORKING:received'); tries++) await delay(30);
    assert.ok(tmux('capture-pane', '-pt', pane).split('\n').includes('WORKING:received'));
    state = 'blocked';
    await assert.rejects(node.sendPrompt('agent', thread, 'ignored', entry.pane.key), { status: 409 });
    await assert.rejects(node.transcript('agent', null, 'a different thread'), { status: 409 });
  } finally {
    node.close();
    tmux('kill-server');
  }
});

test('an empty live Codex Pane accepts the first prompt and enforces the thread once it binds', async (t) => {
  const socket = join(await scratch(t), 't');
  const tmux = (...args) => execFileSync('tmux', ['-S', socket, ...args], { encoding: 'utf8', timeout: 4000 }).trim();
  const pane = tmux('-f', '/dev/null', 'new-session', '-ds', 'fixture', '-PF', '#{pane_id}', 'exec /bin/sh -i');
  const session = tmux('display-message', '-p', '-t', pane, '#{session_id}');
  const pid = Number(tmux('display-message', '-p', '-t', pane, '#{pane_pid}'));
  const started = execFileSync('/bin/ps', ['-o', 'lstart=', '-p', String(pid)], {
    encoding: 'utf8', env: { ...process.env, LC_ALL: 'C', TZ: 'UTC' },
  }).trim();
  let started_at_ticks = Date.parse(started + ' UTC') / 1000;
  let thread = null;
  let kind = 'codex';
  const node = new LocalNode({ socket });
  node.history = new History(await scratch(t));
  node.snapshot = async () => ({
    sessions: [],
    agents: [{
      instance_id: 'agent', kind, hook_session_id: thread, display_state: 'idle',
      process: { pid, started_at_ticks },
      target: { pane_id: pane, session_id: session },
    }],
  });
  try {
    const [entry] = (await node.catalog()).agents;
    assert.equal(entry.chat, true);
    assert.equal(entry.structured, false);
    assert.deepEqual(await node.transcript('agent', null, null), { messages: [], before: null });
    await assert.rejects(node.sendPrompt('agent', null, 'ignored', '0.0.0.0'), { status: 409 });
    const requestId = 'c5a27de3-936f-446e-9de1-797eaa1cfc99';
    const text = "printf 'FIRST:%s\\n' received";
    const receipt = await node.sendPrompt('agent', null, text, entry.pane.key, requestId);
    assert.ok(Number.isFinite(receipt.submittedAt));
    for (let tries = 0; tries < 20 && !tmux('capture-pane', '-pt', pane).split('\n').includes('FIRST:received'); tries++) await delay(30);
    assert.ok(tmux('capture-pane', '-pt', pane).split('\n').includes('FIRST:received'));
    thread = 'c5a27de3-936f-446e-9de1-797eaa1cfc00';
    assert.deepEqual(await node.sendPrompt('agent', null, text, entry.pane.key, requestId), receipt);
    assert.equal(tmux('capture-pane', '-pt', pane).split('\n').filter((line) => line === 'FIRST:received').length, 1);
    await assert.rejects(node.sendPrompt('agent', thread, 'different', entry.pane.key, requestId), { status: 409 });
    await assert.rejects(node.sendPrompt('agent', null, 'ignored', entry.pane.key), { status: 409 });
    assert.deepEqual(await node.transcript('agent', null, thread), { messages: [], before: null });
    started_at_ticks--;
    await assert.rejects(node.sendPrompt('agent', thread, 'ignored', entry.pane.key), { status: 409 });
    kind = 'claude';
    const [changed] = (await node.catalog(true)).agents;
    assert.equal(changed.chat, false);
    await assert.rejects(node.interrupt({ agent: 'agent', thread, pane: entry.pane.key }), { status: 409 });
  } finally {
    node.close();
    tmux('kill-server');
  }
});

test('terminal composer delivers the pasted text followed by one Enter', async (t) => {
  const directory = await scratch(t);
  const socket = join(directory, 't');
  const tmux = (...args) => execFileSync('tmux', ['-S', socket, ...args], { encoding: 'utf8', timeout: 4000 }).trim();
  const pane = tmux('-f', '/dev/null', 'new-session', '-ds', 'fixture', '-c', directory, '-PF', '#{pane_id}',
    "stty raw -echo; printf '\\033[?2004h'; dd bs=1 count=15 of=input 2>/dev/null; exec sleep 20");
  const node = new LocalNode({ socket });
  try {
    for (let tries = 0; tries < 30 && tmux('display-message', '-pt', pane, '#{bracket_paste_flag}') !== '1'; tries++) await delay(25);
    const [target] = await node.panes();
    const { watch } = await node.watchOpen(target.key);
    await node.request('input', { watch, data: 'hi\r', bracketed: true, submit: true });
    let delivered;
    for (let tries = 0; tries < 30; tries++) {
      delivered = await readFile(join(directory, 'input')).catch(() => null);
      if (delivered?.length === 15) break;
      await delay(25);
    }
    assert.deepEqual(delivered, Buffer.from('\x1b[200~hi\x1b[201~\r'));
  } finally {
    node.close();
    tmux('kill-server');
  }
});

test('gateway requires a loopback listener even with TLS configured', async () => {
  const refused = { status: 400, message: 'Use --listen 127.0.0.1:PORT or localhost:PORT.' };
  await assert.rejects(startServer({ origin: 'http://localhost:7080', listen: '0.0.0.0:7080' }), refused);
  await assert.rejects(startServer({
    origin: 'https://workbench.example.net', listen: '0.0.0.0:7080', cert: 'unused', key: 'unused',
  }), refused);
  await assert.rejects(startServer({ origin: 'http://localhost:7080', listen: 'localhost:7080:443' }), refused);
});

test('acknowledging Done only settles the exact completion shown for that agent and pane', async () => {
  const node = new LocalNode({});
  const pane = '1.2.3.4';
  const agent = 'agent-' + 'a'.repeat(24);
  const runtime = 'c5a27de3-936f-446e-9de1-797eaa1cfc00';
  const current = { instance_id: agent, target: { pane_id: '%3' }, display_state: 'done',
    attention: { id: runtime + '.2', kind: 'done', seen: false } };
  node.pane = async (key) => { assert.equal(key, pane); return { pane: '%3' }; };
  node.snapshot = async () => ({ agents: [current] });
  const commands = [];
  node.coreCommand = async (args) => { commands.push(args); };
  await node.request('acknowledge-done', { pane, agent, event: runtime + '.1' });
  await node.request('acknowledge-done', { pane, agent: 'agent-' + 'b'.repeat(24), event: current.attention.id });
  assert.deepEqual(commands, []);
  await node.request('acknowledge-done', { pane, agent, event: current.attention.id });
  current.attention.seen = true;
  await node.request('acknowledge-done', { pane, agent, event: current.attention.id });
  assert.deepEqual(commands, [['attention', 'ack', current.attention.id]]);
});

test('Sidebar pins share tmux options and session shells target the selected session', async (t) => {
  const socket = join(await scratch(t), 't');
  const tmux = (...args) => execFileSync('tmux', ['-S', socket, ...args], { encoding: 'utf8', timeout: 4000 }).trim();
  const anchor = tmux('-f', '/dev/null', 'new-session', '-ds', 'fixture', '-PF', '#{pane_id}');
  const [session, window] = tmux('display-message', '-p', '-t', anchor, '#{session_id} #{window_id}').split(' ');
  const node = new LocalNode({ socket });
  node.snapshot = async () => ({
    agents: [{ instance_id: 'agent', kind: 'codex', target: { pane_id: anchor, session_id: session } }],
    sessions: [{ session_id: session, session_name: 'fixture', last_active_pane_id: anchor }],
    sidebar: { pinnedAgents: [], branches: {} },
  });
  try {
    const before = (await node.catalog()).sessions[0].pane;
    const sidebar = tmux('split-window', '-dPF', '#{pane_id}', '-t', anchor);
    tmux('set-option', '-pt', sidebar, '@pane_role', 'sidebar');
    const sid = tmux('display-message', '-p', '-t', sidebar, '#{pid}.#{start_time}.#{pane_id}.#{pane_pid}').replace('%', '');
    assert.equal((await node.pane(before.key)).pane, anchor);
    await assert.rejects(node.pane(sid), { status: 404 });
    for (const [scope, target, marker] of [['agent', window, '@agent_focus'], ['session', session, '@workbench_session_focus']]) {
      for (const pinned of [true, true, false, false]) {
        await node.pin({ scope, target, pane: before.key, pinned });
        assert.equal(tmux('show-option', scope === 'agent' ? '-wqv' : '-qv', '-t', target, marker), pinned ? '1' : '');
      }
    }
    await assert.rejects(node.create(false, session, 'expired'), { status: 409 });
    const shell = await node.create(false, session, before.key);
    assert.equal(shell.session, session);
    assert.notEqual(shell.window, window);
    assert.equal(tmux('display-message', '-p', '-t', session, '#{window_id}'), window);
    const split = await node.create(true, session, shell.key);
    assert.equal(split.window, shell.window);
    assert.equal(split.resizable, false);
    const catalog = await node.catalog();
    assert.deepEqual(catalog.sessions[0].windows.flatMap(({ panes }) => panes.map(({ pane }) => pane)), [anchor, shell.pane, split.pane]);
    assert.equal(catalog.terminals.filter(({ kind }) => kind === 'shell').length, 2);
  } finally {
    node.close();
    tmux('kill-server');
  }
});

test('locale follows LOCALE, the Workbench override, and system message settings', () => {
  for (const [env, language] of [
    [{}, 'en'],
    [{ LOCALE: 'zh_CN.UTF-8', LC_ALL: 'C' }, 'zh-CN'],
    [{ LOCALE: 'en-GB', TMUX_AGENT_WORKBENCH_LANGUAGE: 'zh' }, 'en'],
    [{ LOCALE: 'auto', TMUX_AGENT_WORKBENCH_LANGUAGE: 'zh-Hans' }, 'zh-CN'],
    [{ LC_ALL: 'C.UTF-8', LC_MESSAGES: 'zh', LANG: 'zh', LANGUAGE: 'zh' }, 'en'],
    [{ LC_MESSAGES: 'zh_CN.UTF-8', LANG: 'en_US.UTF-8' }, 'zh-CN'],
    [{ LANG: 'en_US.UTF-8', LANGUAGE: 'zh:en' }, 'zh-CN'],
    [{ LOCALE: 'fr-FR', LANG: 'zh_CN.UTF-8' }, 'en'],
  ]) assert.equal(resolveLanguage(env), language, JSON.stringify(env));
});

test('gateway serves matching languages for the app shell, manifest, and errors', async (t) => {
  const store = new Store(await scratch(t));
  const local = { cleanup() {}, close() {} };
  const previous = process.env.LOCALE;
  t.after(() => {
    if (previous === undefined) delete process.env.LOCALE;
    else process.env.LOCALE = previous;
  });
  const workers = [];
  for (const [locale, lang, title, error] of [
    ['en-GB', 'en', 'Sign in to Workbench', 'Sign in to Workbench.'],
    ['zh_CN.UTF-8', 'zh-CN', '登录工作台', '请登录工作台。'],
  ]) {
    process.env.LOCALE = locale;
    const gateway = await startServer({ store, local, localName: 'fixture', listen: '127.0.0.1:0', origin: 'http://localhost:7080' });
    const address = 'http://127.0.0.1:' + gateway.address.port;
    try {
      const html = await fetch(address + '/?refresh=1').then((response) => response.text());
      assert.ok(html.includes('<html lang="' + lang + '">'));
      assert.ok(html.includes(title));
      assert.ok(!html.includes('{{'));
      const manifest = await fetch(address + '/manifest.webmanifest').then((response) => response.json());
      assert.equal(manifest.lang, lang);
      assert.equal(manifest.display, 'standalone');
      assert.equal(manifest.theme_color, undefined);
      assert.equal(manifest.background_color, undefined);
      for (const [mode, color] of [['light', '#fafafa'], ['dark', '#282c34']]) {
        const themed = await fetch(address + '/manifest-' + mode + '.webmanifest').then((response) => response.json());
        assert.deepEqual(themed, { ...manifest, theme_color: color, background_color: color });
      }
      assert.ok(html.includes('id="page-theme-color"'));
      assert.ok(html.includes(':root { --paper: #fafafa; }'));
      assert.ok(html.includes('@media (prefers-color-scheme: dark) { :root { --paper: #282c34; } }'));
      assert.ok(html.includes('content="#fafafa" media="(prefers-color-scheme: light)"'));
      assert.ok(html.includes('content="#282c34" media="(prefers-color-scheme: dark)"'));
      const version = html.match(/src="\/app\.js\?v=([a-f0-9]{16})"/)?.[1];
      assert.ok(version);
      assert.ok(html.includes('id="connection-overlay"'));
      assert.ok(html.includes('href="/style.css?v=' + version + '"'));
      const resetHref = '/reset.css?v=' + version;
      assert.ok(html.indexOf(resetHref) > 0 && html.indexOf(resetHref) < html.indexOf('/style.css?v=' + version));
      const reset = await fetch(address + resetHref).then((response) => response.text());
      assert.match(reset, /modern-normalize v3\.0\.1/);
      assert.match(reset, /-webkit-tap-highlight-color: transparent/);
      const style = await fetch(address + '/style.css?v=' + version).then((response) => response.text());
      assert.match(style, /:root \{ color-scheme: light;\s+--paper: #fafafa;/);
      assert.match(style, /:root \{ color-scheme: dark;\s+--paper: #282c34;/);
      assert.ok(style.includes('--terminal-ansi-15:'));
      assert.ok(html.includes('src="/vendor/markdown-it.js?v=' + version + '"'));
      assert.equal((await fetch(address + '/vendor/markdown-it.js?v=' + version)).status, 200);
      const script = await fetch(address + '/app.js?v=' + version).then((response) => response.text());
      assert.ok(script.includes('"language":"' + lang + '"'));
      assert.ok(script.includes("const appVersion = '" + version + "'"));
      assert.ok(!script.includes('WORKBENCH_LOCALE'));
      const protectedWorkspace = await fetch(address + '/api/workspace');
      assert.equal(protectedWorkspace.status, 401);
      assert.deepEqual(await protectedWorkspace.json(), { error });
      const worker = await fetch(address + '/sw.js').then((response) => response.text());
      for (const module of ['chat-state.js', 'connection.js', 'dom.js', 'link-panel.js', 'message-tools.js', 'terminal.js']) {
        assert.ok(script.includes('./' + module + '?v=' + version));
        assert.ok(worker.includes('/' + module));
        assert.equal((await fetch(address + '/' + module + '?v=' + version)).status, 200);
      }
      const panel = await fetch(address + '/link-panel.js?v=' + version).then((response) => response.text());
      assert.ok(panel.includes('./dom.js?v=' + version));
      assert.ok(panel.includes('./link-preview.js?v=' + version));
      assert.ok(worker.includes('/link-preview.js'));
      assert.equal((await fetch(address + '/link-preview.js?v=' + version)).status, 200);
      assert.ok(worker.includes('/vendor/markdown-it.js'));
      assert.ok(worker.includes('/reset.css'));
      workers.push(worker);
    } finally {
      await gateway.close();
    }
  }
  assert.notEqual(...workers);
});

test('gateway protects data and accepts an outbound node handshake only after enrollment', async (t) => {
  const store = new Store(await scratch(t));
  const local = { request: () => ({}), cleanup() {}, close() {} };
  const origin = 'http://localhost:7080';
  const gateway = await startServer({ store, local, localName: 'fixture', origin, listen: 'localhost:0' });
  const address = 'http://127.0.0.1:' + gateway.address.port;
  const controller = new AbortController();
  let connector;
  try {
    assert.equal(gateway.address.address, '127.0.0.1');
    assert.equal((await fetch(address + '/api/workspace')).status, 401);
    const invitationEndpoint = address + '/api/passkeys/invitations';
    for (const [requestOrigin, expected] of [[origin, 401], ['https://untrusted.example', 403]]) {
      for (const endpoint of [invitationEndpoint, address + '/api/close']) {
        const response = await fetch(endpoint, { method: 'POST', headers: { Origin: requestOrigin } });
        assert.equal(response.status, expected);
      }
    }
    assert.deepEqual(await readdir(store.invites), []);
    assert.equal((await fetch(address + '/api/auth/login/options', {
      method: 'POST', headers: { Origin: 'https://untrusted.example' },
    })).status, 403);
    const session = await fetch(address + '/api/session');
    assert.equal(session.headers.get('cache-control'), 'no-store');
    assert.deepEqual(await session.json(), { authenticated: false });
    const app = await fetch(address + '/manifest.webmanifest').then((response) => response.json());
    assert.equal(app.display, 'standalone');
    const crossOrigin = await new Promise((resolve) => {
      const socket = new WebSocket('ws://127.0.0.1:' + gateway.address.port + '/api/terminal', { origin: 'https://untrusted.example' });
      socket.on('unexpected-response', (_request, response) => { response.resume(); resolve(response.statusCode); });
      socket.on('error', () => {});
    });
    assert.equal(crossOrigin, 403);
    const invite = store.issue('node', 'remote');
    const connectorState = join(store.directory, 'fixture-connector');
    const joiner = spawn(process.execPath, [fileURLToPath(new URL('../cli.mjs', import.meta.url)),
      'node', 'join', '--gateway', address, '--state', connectorState],
    { stdio: ['pipe', 'ignore', 'inherit'] });
    const exited = once(joiner, 'close');
    await delay(80);
    joiner.stdin.end(invite + '\n');
    assert.equal((await exited)[0], 0);
    const config = readJson(join(connectorState, 'connector.json'));
    await assert.rejects(enrollNode(address, invite), { status: 403 });
    const connected = new Promise((resolve) => {
      connector = runConnector(config, { ...local, close: resolve }, controller.signal);
    });
    await delay(130);
    assert.equal(store.enrolled(config.id).id, 'remote');
    controller.abort();
    await connected;
  } finally {
    controller.abort();
    await connector;
    await gateway.close();
  }
});
