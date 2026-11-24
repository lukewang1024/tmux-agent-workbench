import { execFile, spawn } from 'node:child_process';
import { once } from 'node:events';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { promisify } from 'node:util';
import { History, hasThread } from './history.mjs';
import { Failure, secret, workbenchState } from './state.mjs';

const run = promisify(execFile);
const webSession = '__workbench_web';
const paneFormat = [
  'pane_id', 'session_id', 'window_id', 'pane_pid', 'pid', 'start_time',
  'pane_width', 'pane_height', 'cursor_x', 'cursor_y', 'pane_dead',
  '@workbench-web-pane', 'pane_in_mode',
].map((field) => '#{' + field + '}').join('|');
const limitsFile = join(workbenchState, 'usage', 'codex-limits.json');
const maxBytes = 384 * 1024;
const ttl = 20_000;

function paneFrom(line) {
  const [pane, session, window, processId, serverId, startTime, cols, rows, x, y, dead, tag, mode] = line.split('|');
  if (!/^%\d+$/.test(pane) || !/^\$\d+$/.test(session) || !/^@\d+$/.test(window)
      || ![processId, serverId, startTime].every((value) => /^\d+$/.test(value)) || dead === '1') return null;
  return {
    pane, session, window, cols: Number(cols), rows: Number(rows), x: Number(x), y: Number(y),
    key: [serverId, startTime, pane.slice(1), processId].join('.'),
    owned: ['window', 'split'].includes(tag), resizable: tag === 'window', inMode: mode === '1',
  };
}

// tmux escapes control bytes and backslashes as octal inside %output notifications.
function terminalBytes(value) {
  const decoded = Buffer.allocUnsafe(value.length);
  let length = 0;
  for (let i = 0; i < value.length; i++) {
    if (value[i] === 92 && i + 3 < value.length && value[i + 1] >= 48 && value[i + 1] <= 51
        && value[i + 2] >= 48 && value[i + 2] <= 55 && value[i + 3] >= 48 && value[i + 3] <= 55) {
      decoded[length++] = (value[i + 1] - 48) * 64 + (value[i + 2] - 48) * 8 + value[i + 3] - 48;
      i += 3;
    } else {
      decoded[length++] = value[i];
    }
  }
  return decoded.subarray(0, length);
}

export class LocalNode {
  watches = new Map();
  recentSnapshot = null;
  refreshingUsage = null;
  refreshedUsage = null;
  history = new History();

  constructor({ socket, core, usage, observeSnapshot = false }) {
    this.socket = socket || process.env.TMUX_AGENT_WORKBENCH_TMUX_SOCKET || process.env.TMUX?.split(',')[0];
    this.core = core || 'tmux-agent-workbench';
    this.usageCommand = usage;
    this.observeSnapshot = observeSnapshot;
    this.env = { ...process.env };
    delete this.env.TMUX;
    delete this.env.TMUX_PANE;
  }

  arguments(args) {
    return this.socket ? ['-S', this.socket, ...args] : args;
  }

  async tmux(args, encoding = 'utf8') {
    const { stdout } = await run('tmux', this.arguments(args), {
      env: this.env, encoding, timeout: 4000, maxBuffer: 2 * 1024 * 1024,
    });
    return stdout;
  }

  async socketPath() {
    return this.socket || (await this.tmux(['display-message', '-p', '#{socket_path}'])).trim();
  }

  async snapshot(fresh = false) {
    if (!fresh && this.recentSnapshot?.expires > Date.now()) return this.recentSnapshot.task;
    const task = this.socketPath().then(async (socket) => {
      const args = ['snapshot', '--json'];
      if (this.observeSnapshot) args.push('--observe');
      const { stdout } = await run(this.core, args, {
        env: { ...this.env, TMUX_AGENT_WORKBENCH_TMUX_SOCKET: socket },
        timeout: 8000, maxBuffer: 4 * 1024 * 1024,
      });
      const data = JSON.parse(stdout);
      if (data.schema_version !== 1 || !Array.isArray(data.agents)) throw new Error('Unsupported Workbench snapshot.');
      return data.agents.filter((agent) => !agent.exited && !agent.stale && agent.target?.pane_id);
    });
    this.recentSnapshot = { expires: Date.now() + 850, task };
    return task;
  }

  async panes() {
    try {
      const rows = await this.tmux(['list-panes', '-a', '-F', paneFormat]);
      return rows.split('\n').map(paneFrom).filter(Boolean);
    } catch {
      return [];
    }
  }

  async catalog(fresh = false) {
    const [snapshot, panes] = await Promise.allSettled([this.snapshot(fresh), this.panes()]);
    const raw = panes.status === 'fulfilled' ? panes.value : [];
    const agents = [];
    for (const agent of snapshot.status === 'fulfilled' ? snapshot.value : []) {
      const target = raw.find((pane) => pane.pane === agent.target.pane_id && pane.session === agent.target.session_id)
        || raw.find((pane) => pane.pane === agent.target.pane_id);
      if (!target) continue;
      agents.push({
        id: agent.instance_id, title: agent.label || agent.target.window_name || agent.kind,
        kind: agent.kind, state: agent.display_state, session: agent.target.session_name,
        pane: target, thread: agent.hook_session_id,
        structured: agent.kind === 'codex' && hasThread(agent.hook_session_id),
      });
    }
    const seen = new Set(agents.map((agent) => agent.pane.key));
    const terminals = agents.map((agent) => ({ pane: agent.pane, title: agent.title, session: agent.session, kind: agent.kind }));
    for (const pane of raw) {
      if (!pane.owned || seen.has(pane.key)) continue;
      seen.add(pane.key);
      terminals.push({ pane, title: 'Shell ' + pane.pane, session: pane.resizable ? 'Web Terminals' : 'Split pane', kind: 'shell' });
    }
    return { agents, terminals, snapshotAvailable: snapshot.status === 'fulfilled' };
  }

  async pane(key, fresh = false) {
    const entry = (await this.catalog(fresh)).terminals.find((item) => item.pane.key === key);
    if (!entry) throw new Failure(404, 'Pane closed or no longer accessible.');
    return entry.pane;
  }

  async verify(target) {
    let current;
    try {
      current = paneFrom((await this.tmux(['display-message', '-p', '-t', target.pane, paneFormat])).trimEnd());
    } catch {
      throw new Failure(404, 'Pane closed or no longer accessible.');
    }
    if (current?.key !== target.key) throw new Failure(409, 'Pane identity changed. Reopen it.');
    return current;
  }

  async transcript(agentId, before) {
    const entry = (await this.catalog()).agents.find((item) => item.id === agentId && item.structured);
    if (!entry) throw new Failure(404, 'No structured history for this running agent.');
    return this.history.page(entry.thread, before);
  }

  async sendPrompt(agentId, thread, text, paneKey) {
    if (typeof text !== 'string' || !text.trim() || Buffer.byteLength(text) > 16_384
        || /[\x00-\x08\x0b-\x0c\x0e-\x1f\x7f]/.test(text)) {
      throw new Failure(400, 'Enter a text message under 16 KB.');
    }
    const entry = (await this.catalog(true)).agents.find((item) => item.id === agentId && item.structured);
    if (!entry || entry.thread !== thread || entry.pane.key !== paneKey) throw new Failure(409, 'The active thread changed. Reopen the agent.');
    if (!['idle', 'done'].includes(entry.state) || entry.pane.inMode) {
      throw new Failure(409, 'The agent is busy or needs attention in the terminal.');
    }
    await this.paste(entry.pane, Buffer.from(text.replaceAll('\r\n', '\n').replaceAll('\r', '\n')), true, async () => {
      const latest = (await this.catalog(true)).agents.find((item) => item.id === agentId);
      if (latest?.thread !== thread || latest.pane.key !== entry.pane.key || latest.pane.inMode
          || !['idle', 'done'].includes(latest.state)) throw new Failure(409, 'The agent can no longer accept this message.');
    });
    return {};
  }

  async paste(pane, bytes, prompt = false, check = null) {
    await this.verify(pane);
    const name = 'workbench-web-' + secret();
    const arguments_ = this.arguments(['load-buffer', '-b', name, '-']);
    const load = spawn('tmux', arguments_, { env: this.env, stdio: ['pipe', 'ignore', 'ignore'] });
    load.stdin.on('error', () => {});
    load.stdin.end(bytes);
    const timer = setTimeout(() => load.kill(), 4000);
    try {
      const [code] = await once(load, 'close');
      if (code !== 0) throw new Failure(502, 'Unable to deliver terminal input.');
      if (check) await check();
      await this.verify(pane);
      const args = ['paste-buffer', '-r', '-d', '-b', name, '-t', pane.pane];
      if (prompt) args.push('-p', ';', 'send-keys', '-t', pane.pane, 'Enter');
      await this.tmux(args);
    } finally {
      clearTimeout(timer);
      await this.tmux(['delete-buffer', '-b', name]).catch(() => {});
    }
  }

  async create(split) {
    let output;
    if (split) {
      const target = await this.pane(split, true);
      await this.verify(target);
      output = await this.tmux(['split-window', '-d', '-P', '-F', '#{pane_id}', '-t', target.pane, '-c', '#{pane_current_path}']);
    } else {
      const sessionArgs = ['new-session', '-d', '-s', webSession, '-n', 'Shell', '-P', '-F', '#{pane_id}'];
      try {
        output = await this.tmux(sessionArgs);
      } catch (error) {
        try { await this.tmux(['has-session', '-t', '=' + webSession]); } catch { throw error; }
        output = await this.tmux(['new-window', '-d', '-t', webSession + ':', '-n', 'Shell', '-P', '-F', '#{pane_id}']);
      }
    }
    const id = output.trim();
    if (!/^%\d+$/.test(id)) throw new Failure(502, 'tmux did not return a Pane ID.');
    try {
      await this.tmux(['set-option', '-p', '-t', id, '@workbench-web-pane', split ? 'split' : 'window']);
      const pane = paneFrom((await this.tmux(['display-message', '-p', '-t', id, paneFormat])).trimEnd());
      if (!pane) throw new Failure(502, 'The new Pane exited before it could be opened.');
      return pane;
    } catch (error) {
      // This ID was returned by the just-completed create operation.
      await this.tmux(['kill-pane', '-t', id]).catch(() => {});
      throw error;
    }
  }

  append(watch, chunk) {
    if (!chunk.length) return;
    watch.frames.push({ seq: ++watch.seq, chunk });
    watch.bytes += chunk.length;
    while (watch.bytes > maxBytes && watch.frames.length) watch.bytes -= watch.frames.shift().chunk.length;
  }

  async screen(watch) {
    const current = await this.verify(watch.pane);
    const text = await this.tmux(['capture-pane', '-p', '-e', '-t', current.pane], 'buffer');
    const lines = text.toString('utf8').trimEnd().replaceAll('\n', '\r\n');
    const data = '\x1b[2J\x1b[H' + lines + '\x1b[0m\x1b[' + (current.y + 1) + ';' + (current.x + 1) + 'H';
    return { initial: Buffer.from(data).toString('base64'), seq: watch.seq, rows: current.rows, cols: current.cols, resizable: current.resizable };
  }

  async watchOpen(key) {
    if (this.watches.size >= 20) throw new Failure(429, 'Too many open terminals.');
    const pane = await this.pane(key);
    const id = secret();
    const stream = spawn('tmux', this.arguments(['-C', 'attach-session', '-f', 'ignore-size', '-t', pane.session]), {
      env: this.env, stdio: ['pipe', 'pipe', 'ignore'],
    });
    const watch = { pane, stream, frames: [], seq: 0, bytes: 0, seen: Date.now(), checked: Date.now(), closed: false, ready: false };
    this.watches.set(id, watch);
    let remaining = Buffer.alloc(0);
    const prefix = Buffer.from('%output ' + pane.pane + ' ');
    stream.on('error', () => { watch.closed = true; });
    stream.on('close', () => { watch.closed = true; });
    stream.stdin.on('error', () => {});
    stream.stdout.on('data', (chunk) => {
      remaining = Buffer.concat([remaining, chunk]);
      let next;
      while ((next = remaining.indexOf(10)) !== -1) {
        const line = remaining.subarray(0, next);
        remaining = remaining.subarray(next + 1);
        if (line.subarray(0, prefix.length).equals(prefix)) this.append(watch, terminalBytes(line.subarray(prefix.length)));
        if (line.subarray(0, 17).toString() === '%session-changed ') watch.ready = true;
      }
      if (remaining.length > maxBytes) this.watchClose(id);
    });
    try {
      for (let attempt = 0; attempt < 80 && !watch.ready && !watch.closed; attempt++) await delay(25);
      if (!watch.ready || watch.closed) throw new Failure(502, 'Unable to attach to the tmux session.');
      return { watch: id, ...await this.screen(watch) };
    } catch (error) {
      this.watchClose(id);
      throw error;
    }
  }

  watchFor(id) {
    const watch = this.watches.get(id);
    if (!watch || watch.closed) throw new Failure(410, 'Terminal disconnected. Reopen it.');
    watch.seen = Date.now();
    return watch;
  }

  async watchRead(id, after) {
    const watch = this.watchFor(id);
    if (Date.now() - watch.checked >= 1200) {
      const current = await this.verify(watch.pane);
      const changed = current.rows !== watch.pane.rows || current.cols !== watch.pane.cols;
      watch.pane = current;
      watch.checked = Date.now();
      if (changed) return this.screen(watch);
    }
    if (after < (watch.frames[0]?.seq || watch.seq + 1) - 1) return this.screen(watch);
    const frames = watch.frames.filter((frame) => frame.seq > after);
    if (!frames.length) return { seq: watch.seq, data: '' };
    return { seq: frames.at(-1).seq, data: Buffer.concat(frames.map((frame) => frame.chunk)).toString('base64') };
  }

  async watchInput(id, data) {
    if (typeof data !== 'string' || !data.length || Buffer.byteLength(data) > 12_288) {
      throw new Failure(400, 'Terminal input exceeds the supported size.');
    }
    await this.paste(this.watchFor(id).pane, Buffer.from(data));
    return {};
  }

  async watchResize(id, cols, rows) {
    const pane = await this.verify(this.watchFor(id).pane);
    if (!pane.resizable || !Number.isInteger(cols) || !Number.isInteger(rows)
         || cols < 24 || cols > 360 || rows < 6 || rows > 160) return {};
    await this.tmux(['resize-window', '-t', pane.window, '-x', String(cols), '-y', String(rows)]);
    return {};
  }

  watchClose(id) {
    const watch = this.watches.get(id);
    this.watches.delete(id);
    watch?.stream.kill();
    return {};
  }

  cleanup() {
    for (const [id, watch] of this.watches) if (watch.closed || Date.now() - watch.seen > ttl) this.watchClose(id);
  }

  close() {
    for (const id of this.watches.keys()) this.watchClose(id);
  }

  async usage(refresh) {
    if (refresh && this.usageCommand) {
      if (!this.refreshingUsage) {
        this.refreshingUsage = run('python3', [this.usageCommand], { env: this.env, timeout: 23_000, maxBuffer: 200_000 })
          .then(({ stdout }) => JSON.parse(stdout)).finally(() => { this.refreshingUsage = null; });
      }
      try {
        this.refreshedUsage = await this.refreshingUsage;
        return { ...this.refreshedUsage, stale: false };
      } catch {
        // Keep the last quota supplied by Workbench's usage cache.
      }
    }
    let cached = this.refreshedUsage;
    try {
      const disk = JSON.parse(await readFile(limitsFile, 'utf8'));
      if (!cached || disk.updated_at > cached.updated_at) cached = disk;
    } catch {}
    return cached ? { ...cached, stale: refresh || Date.now() / 1000 - cached.updated_at > 600 } : null;
  }

  request(method, args) {
    switch (method) {
      case 'catalog': return this.catalog();
      case 'history': return this.transcript(args.agent, args.before);
      case 'prompt': return this.sendPrompt(args.agent, args.thread, args.text, args.pane);
      case 'create': return this.create(args.split);
      case 'open': return this.watchOpen(args.pane);
      case 'read': return this.watchRead(args.watch, args.after);
      case 'input': return this.watchInput(args.watch, args.data);
      case 'resize': return this.watchResize(args.watch, args.cols, args.rows);
      case 'close': return this.watchClose(args.watch);
      case 'usage': return this.usage(args.refresh);
      default: throw new Failure(400, 'Unknown node operation.');
    }
  }
}
