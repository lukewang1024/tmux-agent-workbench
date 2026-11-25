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
  '@workbench-web-pane', 'pane_in_mode', '@pane_role', 'window_index', 'pane_index', 'window_name',
].map((field) => '#{' + field + '}').join('|');
const limitsFile = join(workbenchState, 'usage', 'codex-limits.json');
const maxBytes = 384 * 1024;
const ttl = 20_000;
const sidebarStates = ['blocked', 'done', 'working', 'idle'];
const promptStates = ['idle', 'done', 'working'];
const closeStates = ['idle', 'done'];

function closableAgent(agent) {
  return !agent.exited && !agent.stale && Boolean(agent.process)
    && agent.base_state === 'idle' && closeStates.includes(agent.display_state);
}

function paneFrom(line) {
  const [pane, session, window, processId, serverId, startTime, cols, rows, x, y, dead, tag, mode, role,
    windowIndex, paneIndex, ...windowName] = line.split('|');
  if (!/^%\d+$/.test(pane) || !/^\$\d+$/.test(session) || !/^@\d+$/.test(window)
      || ![processId, serverId, startTime].every((value) => /^\d+$/.test(value)) || dead === '1') return null;
  return {
    pane, session, window, cols: Number(cols), rows: Number(rows), x: Number(x), y: Number(y),
    key: [serverId, startTime, pane.slice(1), processId].join('.'),
    owned: ['window', 'split'].includes(tag), resizable: tag === 'window', inMode: mode === '1',
    sidebar: role === 'sidebar',
    windowIndex: Number(windowIndex), index: Number(paneIndex), windowName: windowName.join('|'),
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
  recentThreads = new WeakMap();
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

  async coreCommand(args) {
    const socket = await this.socketPath();
    const { stdout } = await run(this.core, args, {
      env: { ...this.env, TMUX_AGENT_WORKBENCH_TMUX_SOCKET: socket },
      timeout: 8000, maxBuffer: 4 * 1024 * 1024,
    });
    return stdout;
  }

  snapshot(fresh = false, hidden = []) {
    const states = sidebarStates.filter((state) => hidden.includes(state));
    const key = states.join(',');
    if (!fresh && this.recentSnapshot?.key === key && this.recentSnapshot.expires > Date.now()) return this.recentSnapshot.task;
    const task = (async () => {
      const args = ['snapshot', '--json'];
      if (this.observeSnapshot) args.push('--observe');
      let stdout;
      try {
        stdout = await this.coreCommand([...args, '--sidebar', ...states.flatMap((state) => ['--hide-state', state])]);
      } catch (error) {
        // Older cores still provide the normal agent and session catalog.
        if (!/(?:unexpected|unknown) argument '--sidebar'/.test(error.stderr || '')) throw error;
        stdout = await this.coreCommand(args);
      }
      const data = JSON.parse(stdout);
      if (data.schema_version !== 1 || !Array.isArray(data.agents) || !Array.isArray(data.sessions)) {
        throw new Error('Unsupported Workbench snapshot.');
      }
      return data;
    })();
    this.recentSnapshot = { key, expires: Date.now() + 850, task };
    return task;
  }

  async panes() {
    const rows = await this.tmux(['list-panes', '-a', '-F', paneFormat]);
    return rows.split('\n').map(paneFrom).filter((pane) => pane && !pane.sidebar);
  }

  async catalog(fresh = false, hidden = []) {
    const [snapshot, panes] = await Promise.allSettled([this.snapshot(fresh, hidden), this.panes()]);
    const raw = panes.status === 'fulfilled' ? panes.value : [];
    const data = snapshot.status === 'fulfilled' ? snapshot.value : null;
    const sidebar = data?.sidebar;
    const byPane = new Map(raw.map((pane) => [pane.pane, pane]));
    const byLocation = new Map(raw.map((pane) => [pane.session + '/' + pane.pane, pane]));
    const protectedPanes = new Set((data?.agents || []).filter((agent) => !agent.exited && !closableAgent(agent))
      .map((agent) => agent.target.pane_id));
    const sessionWindows = new Map();
    for (const pane of raw) {
      pane.canClose = Boolean(data) && !protectedPanes.has(pane.pane);
      if (!sessionWindows.has(pane.session)) sessionWindows.set(pane.session, new Map());
      const windows = sessionWindows.get(pane.session);
      if (!windows.has(pane.window)) windows.set(pane.window, { id: pane.window, index: pane.windowIndex, title: pane.windowName, panes: [] });
      windows.get(pane.window).panes.push(pane);
    }
    const agents = [];
    const missingThreads = [];
    for (const agent of data?.agents || []) {
      if (!agent.target?.pane_id || (!sidebar && (agent.exited || agent.stale))) continue;
      const target = byLocation.get(agent.target.session_id + '/' + agent.target.pane_id)
        || byPane.get(agent.target.pane_id);
      if (!target) continue;
      if (!agent.exited && !agent.stale && agent.kind === 'codex' && !hasThread(agent.hook_session_id)) {
        missingThreads.push(agent);
      }
      agents.push({
        id: agent.instance_id, title: agent.label || agent.target.window_name || agent.kind,
        kind: agent.kind, state: agent.display_state, session: agent.target.session_name, pane: target,
        exited: agent.exited, stale: agent.stale, canClose: closableAgent(agent), thread: agent.hook_session_id,
        structured: !agent.exited && !agent.stale && agent.kind === 'codex' && hasThread(agent.hook_session_id),
      });
    }
    if (missingThreads.length) {
      let task = this.recentThreads.get(data);
      if (!task) {
        task = this.history.activeThreads(missingThreads).catch(() => new Map());
        this.recentThreads.set(data, task);
      }
      const threads = await task;
      for (const agent of agents) {
        if (!threads.has(agent.id)) continue;
        agent.thread = threads.get(agent.id);
        agent.structured = true;
      }
    }
    const sessions = (data?.sessions || []).map((session) => {
      const id = session.session_id;
      return {
        id, title: session.session_name, state: session.rollup_state, agentCount: session.agent_count,
        attentionCount: session.attention_count, branch: sidebar?.branches?.[session.current_path],
        windows: [...(sessionWindows.get(id)?.values() || [])],
        pane: byLocation.get(id + '/' + session.last_active_pane_id) || raw.find((pane) => pane.session === id),
      };
    });
    const pinnedAgents = (sidebar?.pinnedAgents || []).map((card) => {
      const agent = agents.find((entry) => entry.id === card.agent);
      const pane = agent?.pane || byLocation.get(card.session + '/' + card.pane)
        || raw.find((target) => target.session === card.session && target.window === card.window);
      return {
        id: agent?.id || card.window, title: agent?.title || card.title, pane, state: card.state,
        kind: agent?.kind || 'shell', session: sessions.find(({ id }) => id === card.session)?.title,
        structured: agent?.structured, canClose: agent?.canClose, thread: agent?.thread, assistant: card.assistant,
      };
    });
    const terminals = agents.filter(({ pane }) => !pane.owned)
      .map((agent) => ({ pane: agent.pane, title: agent.title, session: agent.session, kind: agent.kind }));
    terminals.push(...raw.filter(({ owned }) => owned).map((pane) => ({ pane, title: 'Shell ' + pane.pane, kind: 'shell' })));
    const catalog = {
      agents, sessions, terminals, pinnedAgents,
      snapshotAvailable: Boolean(data) && panes.status === 'fulfilled',
    };
    if (sidebar) {
      const { sort, colors, stats, agentRows, sessionRows, pinnedSessions, launchTool } = sidebar;
      catalog.sidebar = { sort, colors, stats, agentRows, sessionRows, pinnedSessions, launchTool };
    }
    return catalog;
  }

  async pane(key) {
    const pane = (await this.panes().catch(() => [])).find((item) => item.key === key);
    if (!pane) throw new Failure(404, 'Pane closed or no longer accessible.');
    return pane;
  }

  async verify(target) {
    let current;
    try {
      current = paneFrom((await this.tmux(['display-message', '-p', '-t', target.pane, paneFormat])).trimEnd());
    } catch {
      throw new Failure(404, 'Pane closed or no longer accessible.');
    }
    if (current?.key !== target.key || current.sidebar) throw new Failure(409, 'Pane identity changed. Reopen it.');
    return current;
  }

  async closeTarget({ scope, pane, agent }) {
    if (typeof pane !== 'string' || !/^\d+(?:\.\d+){3}$/.test(pane) || !['agent', 'pane'].includes(scope)
        || (scope === 'agent' && (typeof agent !== 'string' || !/^agent-[0-9a-f]{24}$/.test(agent)))) {
      throw new Failure(400, 'Invalid close request.');
    }
    const target = await this.pane(pane);
    const args = ['close-target', '--pane', target.pane, '--key', target.key];
    if (scope === 'agent') args.push('--agent', agent);
    if (this.observeSnapshot) args.push('--observe');
    try {
      await this.coreCommand(args);
    } catch (error) {
      const reason = (error.stderr || '').trim().replace(/^tmux-agent-workbench: /, '');
      if (reason === 'Only idle or done agents can be closed.'
          || reason === 'The close target changed. Refresh the list and try again.') {
        throw new Failure(409, reason);
      }
      throw new Failure(502, 'Could not close the target.');
    } finally {
      this.recentSnapshot = null;
    }
    return {};
  }

  async pin({ scope, target, pane, pinned }) {
    if (!['agent', 'session'].includes(scope) || typeof pinned !== 'boolean') throw new Failure(400, 'Invalid pin request.');
    const catalog = await this.catalog(true);
    if (!catalog.sidebar) throw new Failure(503, 'Sidebar state is unavailable on this machine.');
    const session = scope === 'session';
    const entry = (session ? catalog.sessions : [...catalog.agents, ...catalog.pinnedAgents])
      .find((item) => item.pane?.key === pane && (session ? item.id : item.pane.window) === target);
    if (!entry) throw new Failure(409, 'The target changed. Refresh the list and try again.');
    if (!session && !pinned && catalog.pinnedAgents.some((item) => item.pane?.window === target && item.assistant)) {
      throw new Failure(409, 'The personal assistant stays pinned.');
    }
    const live = await this.verify(entry.pane);
    if ((session ? live.session : live.window) !== target) throw new Failure(409, 'The target changed. Refresh the list and try again.');
    const marker = session ? '@workbench_session_focus' : '@agent_focus';
    await this.tmux(['set-option', ...(session ? [] : ['-w']), ...(pinned ? [] : ['-qu']),
      '-t', target, marker, ...(pinned ? ['1'] : [])]);
    this.recentSnapshot = null;
    return {};
  }

  async setSort(sort) {
    if (!['grouped', 'prioritized'].includes(sort)) throw new Failure(400, 'Invalid Sidebar sort.');
    await this.coreCommand(['sidebar-sort', '--sort', sort]);
    this.recentSnapshot = null;
    return {};
  }

  async launch({ session, pane }) {
    const catalog = await this.catalog(true);
    if (!catalog.sidebar?.launchTool) throw new Failure(503, 'No Agent launcher is available on this machine.');
    const target = catalog.sessions.find((item) => item.id === session && item.pane?.key === pane);
    if (!target) throw new Failure(409, 'The target changed. Refresh the list and try again.');
    const live = await this.verify(target.pane);
    if (live.session !== session) throw new Failure(409, 'The target changed. Refresh the list and try again.');
    let result;
    try {
      result = JSON.parse(await this.coreCommand(['web-launch-agent', '--pane', live.pane, '--session', live.session]));
    } catch {
      throw new Failure(502, 'Could not start the Agent. Check the machine before trying again.');
    }
    this.recentSnapshot = null;
    if (!/^%\d+$/.test(result.pane || '')) return { pane: null };
    const created = (await this.panes().catch(() => [])).find((item) => item.pane === result.pane && item.session === session);
    if (!created || created.key.split('.').slice(0, 2).join('.') !== live.key.split('.').slice(0, 2).join('.')) {
      return { pane: null };
    }
    return { pane: created };
  }

  async transcript(agentId, before, thread) {
    const entry = (await this.catalog(true)).agents.find((item) => item.id === agentId && item.structured);
    if (!entry) throw new Failure(404, 'No structured history for this running agent.');
    if (thread && entry.thread !== thread) throw new Failure(409, 'The active thread changed. Reopen the agent.');
    return this.history.page(entry.thread, before);
  }

  async sendPrompt(agentId, thread, text, paneKey) {
    if (typeof text !== 'string' || !text.trim() || Buffer.byteLength(text) > 16_384
        || /[\x00-\x08\x0b-\x0c\x0e-\x1f\x7f]/.test(text)) {
      throw new Failure(400, 'Enter a text message under 16 KB.');
    }
    const entry = (await this.catalog(true)).agents.find((item) => item.id === agentId && item.structured);
    if (!entry || entry.thread !== thread || entry.pane.key !== paneKey) throw new Failure(409, 'The active thread changed. Reopen the agent.');
    if (!promptStates.includes(entry.state) || entry.pane.inMode) {
      throw new Failure(409, 'The Agent cannot accept input right now.');
    }
    let after;
    await this.paste(entry.pane, Buffer.from(text.replaceAll('\r\n', '\n').replaceAll('\r', '\n')), {
      bracketed: true,
      submit: true,
      check: async () => {
        const latest = (await this.catalog(true)).agents.find((item) => item.id === agentId);
        if (!latest?.structured || latest.thread !== thread || latest.pane.key !== entry.pane.key || latest.pane.inMode
            || !promptStates.includes(latest.state)) throw new Failure(409, 'The agent can no longer accept this message.');
        after = await this.history.endOffset(thread).catch(() => undefined);
      },
    });
    return { after };
  }

  async paste(pane, bytes, { bracketed = false, submit = false, check } = {}) {
    await this.verify(pane);
    if (!bytes.length && submit) {
      await this.tmux(['send-keys', '-t', pane.pane, 'Enter']);
      return;
    }
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
      if (bracketed) args.push('-p');
      if (submit) args.push(';', 'send-keys', '-t', pane.pane, 'Enter');
      await this.tmux(args);
    } finally {
      clearTimeout(timer);
      await this.tmux(['delete-buffer', '-b', name]).catch(() => {});
    }
  }

  async create(split, session, anchor) {
    let output;
    if (session) {
      const pane = (await this.panes()).find((item) => item.session === session && item.key === anchor);
      if (!pane || typeof split !== 'boolean') throw new Failure(409, 'The target changed. Refresh the list and try again.');
      const live = await this.verify(pane);
      if (live.session !== session) throw new Failure(409, 'The target changed. Refresh the list and try again.');
      const placement = split ? ['split-window', '-t', pane.pane] : ['new-window', '-t', session + ':', '-n', 'Shell'];
      output = await this.tmux([...placement, '-d', '-P', '-F', '#{pane_id}', '-c', '#{pane_current_path}']);
    } else if (split) {
      const target = await this.pane(split);
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

  async watchInput(id, data, { bracketed = false, submit = false } = {}) {
    if (typeof data !== 'string' || (!data.length && !submit) || Buffer.byteLength(data) > 12_288) {
      throw new Failure(400, 'Terminal input exceeds the supported size.');
    }
    await this.paste(this.watchFor(id).pane, Buffer.from(data), { bracketed: bracketed || submit, submit });
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
      case 'catalog': return this.catalog(false, Array.isArray(args.hidden) ? args.hidden : []);
      case 'pin': return this.pin(args);
      case 'close-target': return this.closeTarget(args);
      case 'sidebar-sort': return this.setSort(args.sort);
      case 'launch': return this.launch(args);
      case 'history': return this.transcript(args.agent, args.before, args.thread);
      case 'prompt': return this.sendPrompt(args.agent, args.thread, args.text, args.pane);
      case 'create': return this.create(args.split, args.session, args.anchor);
      case 'open': return this.watchOpen(args.pane);
      case 'read': return this.watchRead(args.watch, args.after);
      case 'input': {
        const submit = args.submit === true && typeof args.data === 'string' && args.data.endsWith('\r');
        const data = submit ? args.data.slice(0, -1) : args.data;
        return this.watchInput(args.watch, data, { bracketed: args.bracketed === true, submit });
      }
      case 'resize': return this.watchResize(args.watch, args.cols, args.rows);
      case 'close': return this.watchClose(args.watch);
      case 'usage': return this.usage(args.refresh);
      default: throw new Failure(400, 'Unknown node operation.');
    }
  }
}
