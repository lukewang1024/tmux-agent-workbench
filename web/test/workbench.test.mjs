import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { WebSocket } from 'ws';
import { enrollNode, runConnector } from '../connector.mjs';
import { History } from '../history.mjs';
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

test('Codex history is tied to an exact rollout and only returns visible text', async (t) => {
  const directory = await scratch(t);
  const session = 'c5a27de3-936f-446e-9de1-797eaa1cfc00';
  const target = join(directory, 'sessions', '2026', '01', '01');
  await mkdir(target, { recursive: true });
  const message = (text, role) => ({ type: 'response_item', payload: { type: 'message', role, content: [{ type: role === 'user' ? 'input_text' : 'output_text', text }] } });
  const rows = [{ type: 'session_meta', payload: { id: session } }];
  rows.push(message('system instructions', 'developer'), message('<environment_context>private</environment_context>', 'user'));
  for (let index = 0; index < 50; index++) rows.push(message('message ' + index, index % 2 ? 'assistant' : 'user'));
  await writeFile(join(target, 'rollout-date-' + session + '.jsonl'), rows.map((row) => JSON.stringify(row)).join('\n') + '\n{"partial"');
  const history = new History(directory);
  const latest = await history.page(session);
  assert.equal(latest.messages.length, 35);
  assert.equal(latest.messages.at(-1).text, 'message 49');
  assert.ok(latest.before);
  const earlier = await history.page(session, latest.before);
  assert.equal(earlier.messages.length, 15);
  assert.equal(earlier.messages.at(0).text, 'message 0');
  assert.equal(earlier.before, null);
  await assert.rejects(history.page('../wrong'), { status: 409 });
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

test('gateway protects data and accepts an outbound node handshake only after enrollment', async (t) => {
  const store = new Store(await scratch(t));
  const local = { request: () => ({}), cleanup() {}, close() {} };
  const origin = 'http://localhost:7080';
  const gateway = await startServer({ store, local, localName: 'fixture', origin, listen: '127.0.0.1:0' });
  const address = 'http://127.0.0.1:' + gateway.address.port;
  const controller = new AbortController();
  let connector;
  try {
    assert.equal((await fetch(address + '/api/workspace')).status, 401);
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
