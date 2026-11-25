import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, mkdtemp, open, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { WebSocket } from 'ws';
import { enrollNode, runConnector } from '../connector.mjs';
import { History } from '../history.mjs';
import { resolveLanguage } from '../i18n.mjs';
import { startServer } from '../server.mjs';
import { hash, readJson, Store } from '../state.mjs';
import { LocalNode } from '../tmux.mjs';

async function scratch(t) {
  const directory = await mkdtemp(join(tmpdir(), 'wb-web-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  return directory;
}

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
  await writeFile(join(target, 'rollout-date-' + session + '.jsonl'), rows.map((row) => JSON.stringify(row)).join('\n') + '\n{"partial"');
  const history = new History(directory);
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
  node.snapshot = async () => ({
    sessions: [],
    agents: [{
      instance_id: 'agent', kind: 'codex', hook_session_id: thread, display_state: state,
      target: { pane_id: pane, session_id: session },
    }],
  });
  try {
    const [entry] = (await node.catalog()).agents;
    await node.sendPrompt('agent', thread, "printf 'WORKING:%s\\n' received", entry.pane.key);
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
      const version = html.match(/src="\/app\.js\?v=([a-f0-9]{16})"/)?.[1];
      assert.ok(version);
      assert.ok(html.includes('href="/style.css?v=' + version + '"'));
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
      assert.ok(worker.includes('/vendor/markdown-it.js'));
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
