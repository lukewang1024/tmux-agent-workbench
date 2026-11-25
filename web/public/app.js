'use strict';

const { language, messages } = WORKBENCH_LOCALE;
const plurals = new Intl.PluralRules(language);
const percentage = new Intl.NumberFormat(language, { style: 'percent' });
const byId = (id) => document.getElementById(id);
const app = byId('app');
const overlay = byId('overlay');
const mobile = window.matchMedia('(max-width: 767px)');
const chatInput = byId('chat-input');
const termInput = byId('term-input');
const nav = [...document.querySelectorAll('.nav-button')];
const passkeys = window.SimpleWebAuthnBrowser;
const loopback = location.protocol === 'http:' && location.hostname === 'localhost';
const connectionError = t(loopback ? 'sshTunnelUnavailable' : 'gatewayUnavailable');
const stateNames = { idle: t('idle'), blocked: t('attention'), working: t('working'), done: t('done'), unknown: t('unknownStatus') };
const nodeStates = { pending: t('pending'), offline: t('offline'), online: t('online') };
const kindNames = { codex: 'Codex', claude: 'Claude Code', opencode: 'OpenCode', trae: 'Trae', shell: 'Shell' };
const marks = { codex: 'Cx', claude: 'Cl', opencode: 'Oc', trae: 'Tr', shell: '>_' };
const labels = { agents: t('agents'), terminals: t('terminals') };

let signedIn = false;
let route = 'login';
let scope = 'all';
let opened = false;
let view = 'chat';
let minePage = 'home';
let machines = [];
let overview = '';
let active = null;
let activeKey = '';
let toastTimer;
let syncing = false;
let installPrompt;
let enrollment = readEnrollment();
const selection = new Map();
const drafts = new Map();
let transcript = null;
let submitting = false;
let terminal = null;

function t(key, values = {}) {
  const message = messages[key];
  const template = typeof message === 'string' ? message : message[plurals.select(values.count)] || message.other;
  return template.replace(/\{(\w+)\}/g, (_match, name) => values[name]);
}

function make(tag, className, text) {
  const element = document.createElement(tag);
  if (className) element.className = className;
  if (text !== undefined) element.textContent = text;
  return element;
}

function button(text, className, action) {
  const element = make('button', className, text);
  element.type = 'button';
  element.addEventListener('click', action);
  return element;
}

function glyph(name) {
  const icon = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  const use = document.createElementNS(icon.namespaceURI, 'use');
  icon.setAttribute('class', 'glyph');
  icon.setAttribute('aria-hidden', 'true');
  use.setAttribute('href', '#icon-' + name);
  icon.append(use);
  return icon;
}

function status(id, message = '') {
  const element = byId(id);
  element.textContent = message;
  element.hidden = !message;
}

function toast(message) {
  clearTimeout(toastTimer);
  status('toast', message);
  toastTimer = setTimeout(() => status('toast'), 4800);
}

function readEnrollment() {
  const invite = new URLSearchParams(location.hash.slice(1)).get('invite');
  if (invite) history.replaceState(history.state, '', location.pathname + location.search);
  return invite;
}

async function api(path, method = 'GET', payload = null) {
  const response = await fetch(path, {
    method,
    headers: method === 'GET' ? undefined : { 'content-type': 'application/json' },
    body: method === 'GET' ? undefined : JSON.stringify(payload || {}),
    cache: 'no-store',
    signal: AbortSignal.timeout(path === '/api/usage' ? 27_000 : 13_000),
  }).catch(() => { throw new Error(connectionError); });
  const data = await response.json();
  if (!response.ok) {
    if (response.status === 401 && signedIn) signedOut();
    const error = new Error(data.error || t('requestFailed'));
    error.status = response.status;
    throw error;
  }
  return data;
}

function pathWith(path, query) {
  return path + '?' + new URLSearchParams(query);
}

function sheet(title, ...contents) {
  byId('sheet-title').textContent = title;
  byId('sheet-body').replaceChildren(...contents);
  if (!overlay.open) overlay.showModal();
  byId('close').focus();
}

function closeSheet() {
  if (overlay.open) overlay.close();
}

function isWorkspace() { return route === 'agents' || route === 'terminals'; }
function recordKey(item) { return item ? item.node + '/' + item.pane.key + '/' + item.id + '/' + (item.thread || '') : ''; }
function focus(id) { byId(id).focus({ preventScroll: true }); }

function save(method, up = false) {
  history[method]({ workbench: true, route, scope, opened, minePage, view, selected: activeKey, up }, '');
}

function navigate(target, node = scope) {
  if (target === route && node === scope) {
    if ((isWorkspace() && opened) || (route === 'mine' && minePage !== 'home')) back();
    return;
  }
  closeSheet();
  route = target;
  scope = node;
  opened = false;
  minePage = 'home';
  paint();
  save('pushState');
  focus(isWorkspace() ? 'list-heading' : route === 'mine' ? 'mine-heading' : 'machines-heading');
  if (route === 'mine') loadUsage();
}

function back() {
  if (history.state?.up) { history.back(); return; }
  opened = false;
  minePage = 'home';
  paint();
  save('replaceState');
  focus(route === 'mine' ? 'mine-heading' : 'list-heading');
}

function paint() {
  app.dataset.route = route;
  byId('workspace').hidden = !isWorkspace();
  for (const name of ['machines', 'mine', 'login']) byId(name).hidden = name !== route;
  for (const item of nav) {
    if (item.dataset.route === route) item.setAttribute('aria-current', 'page');
    else item.removeAttribute('aria-current');
  }
  if (isWorkspace()) paintWorkspace();
  else {
    stopTerminal();
    if (route === 'machines') paintMachines();
    if (route === 'mine') paintMine();
  }
}

function scopedMachines() {
  return scope === 'all' ? machines : machines.filter(({ id }) => id === scope);
}

function entries(machine) {
  const rows = route === 'terminals' ? machine.terminals : machine.agents;
  return (rows || []).map((item) => ({
    ...item,
    session: item.kind === 'shell' ? t(item.pane.resizable ? 'webTerminals' : 'splitPane') : item.session,
    id: item.id || item.pane.key,
    node: machine.id,
    host: machine.name,
  }));
}

function paintWorkspace() {
  const rows = scopedMachines().flatMap(entries);
  const previous = activeKey;
  active = rows.find((item) => recordKey(item) === selection.get(route)) || rows[0] || null;
  activeKey = recordKey(active);
  if (previous !== activeKey) {
    view = route === 'agents' && active?.structured ? 'chat' : 'terminal';
    transcript = null;
    status('chat-error');
    stopTerminal();
  }
  if (!active) opened = false;
  byId('workspace').dataset.mobileView = opened ? 'detail' : 'list';
  paintCollection();
  paintDetail();
}

function paintCollection() {
  const selected = machines.find(({ id }) => id === scope);
  byId('scope').textContent = selected?.name || t('allMachines');
  byId('health').textContent = selected ? nodeStates[selected.state] : t('onlineCount', {
    count: machines.filter(({ state }) => state === 'online').length,
  });
  byId('list-heading').textContent = labels[route];
  const groups = byId('groups');
  const focusKey = document.activeElement?.dataset.record;
  groups.replaceChildren();

  for (const machine of scopedMachines()) {
    const group = make('section', 'group');
    const rows = entries(machine);
    const heading = make('h3');
    const count = machine.state === 'online'
      ? t(route === 'agents' ? 'agentCount' : 'paneCount', { count: rows.length }) : nodeStates[machine.state];
    heading.append(make('span', machine.state === 'online' ? 'dot' : 'dot off'), machine.name, make('span', 'count', count));
    group.append(heading);
    if (!rows.length) {
      const message = machine.state === 'pending' ? 'waitingForPairing' : machine.state === 'offline' ? 'nodeReconnect'
        : machine.snapshotAvailable === false && route === 'agents' ? 'snapshotUnavailable' : 'noSessions';
      group.append(make('div', 'empty', t(message)));
    }
    for (const item of rows) {
      const row = button('', 'agent-row', () => openRecord(item));
      row.dataset.record = recordKey(item);
      if (row.dataset.record === activeKey) row.setAttribute('aria-current', 'true');
      const icon = make('span', 'agent-tag ' + item.kind, marks[item.kind] || '·');
      icon.setAttribute('aria-hidden', 'true');
      const copy = make('span');
      copy.append(make('b', '', item.title), make('span', 'agent-meta', (item.session || '') + ' / ' + (kindNames[item.kind] || item.kind)));
      row.append(icon, copy, make('span', 'status ' + (item.state || ''), stateNames[item.state] || ''));
      group.append(row);
      if (row.dataset.record === focusKey) requestAnimationFrame(() => row.focus({ preventScroll: true }));
    }
    groups.append(group);
  }
  if (!machines.length) groups.append(make('div', 'empty', t('loadingWorkspace')));
}

function openRecord(item) {
  const fromList = !opened;
  opened = true;
  selection.set(route, recordKey(item));
  paintWorkspace();
  save('pushState', fromList);
  if (mobile.matches) focus('detail-name');
}

function paintDetail() {
  const detail = byId('detail');
  detail.hidden = false;
  detail.querySelector('.detail-bar').hidden = !active;
  byId('detail-empty').hidden = Boolean(active);
  if (!active) {
    byId('detail-empty').textContent = t('chooseMachine');
    byId('detail-tabs').hidden = true;
    byId('chat').hidden = true;
    byId('term').hidden = true;
    return;
  }
  byId('detail-name').textContent = active.title;
  byId('detail-context').textContent = active.host + ' / ' + (active.session || kindNames[active.kind] || '');
  byId('back').setAttribute('aria-label', t(route === 'agents' ? 'backToAgents' : 'backToTerminals'));
  const canChat = route === 'agents' && active.structured;
  byId('detail-tabs').hidden = !canChat;
  if (!canChat) view = 'terminal';
  const inChat = view === 'chat';
  byId('chat').hidden = !inChat;
  byId('term').hidden = inChat;
  for (const [id, selected] of [['chat-tab', inChat], ['term-tab', !inChat]]) {
    byId(id).setAttribute('aria-selected', String(selected));
    byId(id).tabIndex = selected ? 0 : -1;
  }
  if (!opened && mobile.matches) { stopTerminal(); return; }
  if (inChat) {
    stopTerminal();
    paintComposer();
    if (transcript?.key !== activeKey) {
      transcript = { key: activeKey, rows: new Map(), before: null, loaded: false, busy: false };
      byId('messages').replaceChildren(make('p', 'day', t('readingHistory')));
      byId('older').hidden = true;
      loadHistory();
    }
  } else startTerminal();
}

function tab(next) {
  if (view === next) return;
  view = next;
  paintDetail();
  save('replaceState', history.state?.up || false);
}

function paintComposer() {
  if (chatInput.dataset.thread !== activeKey) {
    chatInput.dataset.thread = activeKey;
    chatInput.value = drafts.get(activeKey) || '';
  }
  const ready = ['idle', 'done'].includes(active.state) && !active.pane.inMode;
  byId('send').disabled = !ready || submitting;
  byId('chat-status').textContent = t(ready ? 'originalConversation' : active.state === 'blocked' ? 'confirmInTerminal' : 'agentWorking');
}

async function loadHistory(older = false) {
  if (!active || !transcript || transcript.busy || (older && !transcript.before)) return;
  const target = transcript;
  target.busy = true;
  const query = { node: active.node, agent: active.id };
  if (older) query.before = target.before;
  try {
    const { messages, before } = await api(pathWith('/api/history', query));
    if (target !== transcript) return;
    if (!target.loaded || older) target.before = before;
    target.loaded = true;
    for (const message of messages) target.rows.set(message.id, message);
    if (target.rows.size > 800) target.rows.delete(target.rows.keys().next().value);
    renderHistory(target, older);
  } catch (error) {
    if (target !== transcript || !signedIn) return;
    if (!target.loaded) byId('messages').replaceChildren(make('div', 'empty', error.status === 404 ? t('noReadableHistory') : error.message));
  } finally { target.busy = false; }
}

function renderHistory(target, older) {
  const messages = byId('messages');
  const scroller = messages.parentElement;
  const nearBottom = scroller.scrollHeight - scroller.clientHeight - scroller.scrollTop < 60;
  const height = scroller.scrollHeight;
  const top = scroller.scrollTop;
  byId('older').hidden = !target.before;
  if (!target.rows.size) {
    messages.replaceChildren(make('div', 'empty', t('noMessages')));
    return;
  }
  messages.replaceChildren(...[...target.rows.values()].sort((a, b) => a.id - b.id).map((row) => {
    const human = row.role === 'user';
    const element = make('article', human ? 'message human' : 'message');
    element.append(make('p', 'message-meta', human ? t('you') : 'Codex'), make('div', human ? 'bubble' : 'messages-text', row.text));
    return element;
  }));
  if (older) scroller.scrollTop = top + scroller.scrollHeight - height;
  else if (nearBottom) scroller.scrollTop = scroller.scrollHeight;
}

async function send() {
  if (byId('send').disabled || !chatInput.value.trim() || !active) return;
  const item = active;
  const key = activeKey;
  const text = chatInput.value;
  submitting = true;
  paintComposer();
  status('chat-error');
  try {
    await api('/api/prompt', 'POST', { node: item.node, pane: item.pane.key, agent: item.id, thread: item.thread, text });
    if (drafts.get(key) === text) drafts.delete(key);
    if (activeKey === key) {
      if (chatInput.value === text) chatInput.value = '';
      loadHistory();
    }
  } catch (error) {
    if (activeKey === key) status('chat-error', error.message);
  } finally {
    submitting = false;
    if (active?.structured && view === 'chat') paintComposer();
  }
}

function encodeBytes(data) { return Uint8Array.from(atob(data), (character) => character.charCodeAt(0)); }

function stopTerminal() {
  const current = terminal;
  if (!current) return;
  terminal = null;
  clearTimeout(current.retry);
  current.observer?.disconnect();
  current.socket?.close();
  current.xterm?.dispose();
  byId('term-screen').replaceChildren();
}

function startTerminal() {
  if (terminal?.key === activeKey) return;
  stopTerminal();
  const current = { key: activeKey, node: active.node, pane: active.pane, writable: false, ready: false, ctrl: false };
  terminal = current;
  byId('term-target').textContent = 'tmux ' + active.pane.pane;
  termInput.value = '';
  const renderer = new window.Terminal({
    cols: Math.max(1, active.pane.cols), rows: Math.max(1, active.pane.rows), fontSize: 13,
    cursorBlink: false, scrollback: 2000, theme: { background: '#17243a', foreground: '#e2edff', cursor: '#b3c6ff' },
    disableStdin: true, cursorInactiveStyle: 'outline',
  });
  current.xterm = renderer;
  current.fit = new window.FitAddon.FitAddon();
  renderer.loadAddon(current.fit);
  renderer.open(byId('term-screen'));
  renderer.onData((data) => sendTerminal({ type: 'input', data }));
  renderer.attachCustomKeyEventHandler((event) => !(event.ctrlKey && event.shiftKey && /^c$/i.test(event.key)));
  current.observer = new ResizeObserver(() => fitTerminal(current));
  current.observer.observe(byId('term-screen'));
  connectTerminal(current);
}

function connectTerminal(current) {
  if (terminal !== current) return;
  current.ready = false;
  current.writable = false;
  current.ctrl = false;
  terminalControls('connecting');
  const url = new URL(pathWith('/api/terminal', { node: current.node, pane: current.pane.key }), location.href);
  url.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
  const socket = new WebSocket(url);
  current.socket = socket;
  socket.addEventListener('message', (event) => {
    if (terminal !== current) return;
    const packet = JSON.parse(event.data);
    if (packet.error) { toast(packet.error); return; }
    if (packet.initial) {
      current.ready = true;
      current.resizable = packet.resizable;
      current.xterm.resize(Math.max(1, packet.cols), Math.max(1, packet.rows));
      current.xterm.reset();
      current.xterm.write(encodeBytes(packet.initial));
      terminalControls(current.writable ? 'inputEnabled' : 'watching');
    }
    if (packet.data) current.xterm.write(encodeBytes(packet.data));
    if (typeof packet.writable === 'boolean') {
      current.writable = packet.writable;
      current.xterm.options.disableStdin = !current.writable;
      terminalControls(current.writable ? 'inputEnabled' : 'watching');
      if (current.writable) {
        fitTerminal(current);
        if (!mobile.matches) current.xterm.focus();
      }
    }
  });
  socket.addEventListener('close', (event) => {
    if (terminal !== current) return;
    current.writable = false;
    current.ready = false;
    current.xterm.options.disableStdin = true;
    terminalControls('disconnected');
    if (event.code === 4001) { signedOut(); return; }
    current.retry = setTimeout(() => connectTerminal(current), 2000);
  });
  socket.addEventListener('error', () => {});
}

function terminalControls(message) {
  const writable = Boolean(terminal?.writable);
  byId('term-state').replaceChildren(make('span', terminal?.ready ? 'dot' : 'dot off'), t(message));
  byId('control').textContent = t(writable ? 'releaseInput' : 'enableInput');
  byId('control').disabled = !terminal?.ready;
  byId('term-form').hidden = !writable;
  for (const item of document.querySelectorAll('[data-key]')) item.disabled = !writable;
  document.querySelector('[data-key="Ctrl"]').setAttribute('aria-pressed', String(Boolean(terminal?.ctrl && writable)));
}

function fitTerminal(current) {
  if (terminal !== current || !current.resizable || !current.writable) return;
  const dimensions = current.fit.proposeDimensions();
  if (!dimensions || !Number.isFinite(dimensions.cols) || !Number.isFinite(dimensions.rows)) return;
  const cols = Math.max(24, Math.min(360, dimensions.cols));
  const rows = Math.max(6, Math.min(160, dimensions.rows));
  if (cols === current.xterm.cols && rows === current.xterm.rows) return;
  current.xterm.resize(cols, rows);
  sendTerminal({ type: 'resize', cols, rows });
}

function sendTerminal(packet) {
  if (terminal?.socket?.readyState !== WebSocket.OPEN) return;
  if (!terminal.writable && ['input', 'resize'].includes(packet.type)) return;
  if (packet.type === 'input') {
    // Split paste into bounded chunks without bisecting Unicode code points.
    const characters = Array.from(packet.data);
    for (let offset = 0; offset < characters.length; offset += 2500) {
      terminal.socket.send(JSON.stringify({ type: 'input', data: characters.slice(offset, offset + 2500).join('') }));
    }
  } else terminal.socket.send(JSON.stringify(packet));
}

function choice(label, detail, action) {
  const row = button('', 'sheet-row', action);
  row.append(make('span', 'choice', label), make('small', '', detail));
  return row;
}

function switchMachine() {
  const all = { id: 'all', name: t('allMachines') };
  sheet(t('switchMachine'), ...[all, ...machines].map((item) => {
    const row = choice(item.name, nodeStates[item.state] || t('machineCount', { count: machines.length }), () => {
      scope = item.id;
      opened = false;
      closeSheet();
      paintWorkspace();
      save('replaceState');
    });
    row.setAttribute('aria-current', String(scope === item.id));
    if (item.state) row.prepend(make('span', item.state === 'online' ? 'dot' : 'dot off'));
    return row;
  }));
}

function paneSheet(event) {
  const online = machines.filter(({ state }) => state === 'online');
  if (!online.length) { toast(t('noOnlineMachines')); return; }
  const anchor = event.currentTarget.closest('#detail') && active ? active : null;
  const form = make('form', 'pane-form');
  const machine = make('select');
  const placement = make('select');
  for (const item of online) machine.add(new Option(item.name, item.id));
  const preferred = anchor?.node || scope;
  if (online.some(({ id }) => id === preferred)) machine.value = preferred;
  function locations() {
    placement.replaceChildren(new Option(t('newWindow'), 'window'));
    if (anchor && anchor.node === machine.value) placement.add(new Option(t('splitCurrentTask'), 'split'));
  }
  locations();
  machine.addEventListener('change', locations);
  const machineLabel = make('label', '', t('machine'));
  machineLabel.append(machine);
  const locationLabel = make('label', '', t('placement'));
  locationLabel.append(placement);
  const submit = make('button', 'primary', t('createShellPane'));
  submit.type = 'submit';
  form.append(machineLabel, locationLabel, submit);
  form.addEventListener('submit', async (input) => {
    input.preventDefault();
    submit.disabled = true;
    const id = machine.value;
    try {
      const pane = await api('/api/panes', 'POST', { node: id, split: placement.value === 'split' ? anchor.pane.key : null });
      closeSheet();
      route = 'terminals';
      if (scope !== 'all') scope = id;
      opened = true;
      selection.set(route, id + '/' + pane.key + '/' + pane.key + '/');
      await refresh(true);
      paint();
      save('pushState');
      if (active) focus('detail-name');
    } catch (error) { closeSheet(); toast(error.message); }
  });
  sheet(t('openPane'), form);
}

function paintMachines() {
  byId('machine-summary').textContent = t('machineSummary', {
    machines: t('machineCount', { count: machines.length }),
    online: t('onlineCount', { count: machines.filter(({ state }) => state === 'online').length }),
  });
  byId('node-list').replaceChildren(...machines.map((item) => {
    const row = button('', 'node-button', () => navigate('agents', item.id));
    const icon = make('span', 'row-glyph');
    icon.append(glyph('machines'));
    const copy = make('span', 'row-copy');
    copy.append(make('b', '', item.name));
    if (item.state === 'online') copy.append(make('small', '', t('agentCount', { count: item.agents?.length || 0 })));
    const trailing = make('span', 'trailing');
    trailing.append(make('span', item.state === 'online' ? 'dot' : 'dot off'), nodeStates[item.state], glyph('next'));
    row.append(icon, copy, trailing);
    return row;
  }));
}

async function refresh(force = false) {
  if (!signedIn || (syncing && !force)) return;
  syncing = true;
  try {
    const result = await api('/api/workspace');
    if (!signedIn) return;
    const fingerprint = JSON.stringify(result);
    if (fingerprint !== overview || force) {
      machines = result.nodes;
      overview = fingerprint;
      if (isWorkspace()) paintWorkspace();
      else if (route === 'machines') paintMachines();
    }
  } catch (error) {
    if (signedIn && force) toast(error.message);
  } finally { syncing = false; }
}

function paintMine() {
  for (const section of ['home', 'keys', 'settings']) byId('mine-' + section).hidden = minePage !== section;
  byId('mine-back').hidden = minePage === 'home';
  byId('mine-heading').textContent = t({ home: 'profile', keys: 'passkeys', settings: 'settings' }[minePage]);
}

function openMine(page) {
  minePage = page;
  paintMine();
  save('pushState', true);
  focus('mine-heading');
  if (page === 'keys') loadKeys();
}

async function loadUsage(refreshNow = false) {
  const target = byId('usage-windows');
  byId('usage-refresh').disabled = true;
  try {
    const result = await api('/api/usage', refreshNow ? 'POST' : 'GET');
    const limits = result?.payload?.rate_limits || {};
    const windows = [limits.primary, limits.secondary, limits.individual_limit].filter(Boolean);
    target.replaceChildren(...windows.map((window) => {
      const remaining = Math.max(0, Math.min(100, Math.round(100 - window.used_percent)));
      const minutes = window.window_minutes;
      const title = minutes === 10080 ? t('weeklyWindow')
        : minutes % 60 === 0 ? t('hourWindow', { count: minutes / 60 }) : t('minuteWindow', { count: minutes });
      const row = make('div', 'metric');
      const head = make('div', 'metric-head');
      head.append(make('span', '', title), make('strong', '', t('quotaRemaining', { percent: percentage.format(remaining / 100) })));
      const meter = make('meter');
      meter.max = 100;
      meter.value = remaining;
      meter.setAttribute('aria-label', t('quotaMeter', { window: title }));
      row.append(head, meter);
      if (window.resets_at) {
        const date = new Date(window.resets_at * 1000).toLocaleString(language);
        row.append(make('p', 'reset', t('quotaReset', { date })));
      }
      return row;
    }));
    byId('usage-note').textContent = windows.length ? (result.stale ? t('lastRecordedUsage') : '') : t('noUsage');
  } catch (error) {
    if (signedIn) byId('usage-note').textContent = error.message;
  } finally { byId('usage-refresh').disabled = false; }
}

async function loadKeys() {
  try {
    const keys = await api('/api/passkeys');
    byId('key-list').replaceChildren(...keys.map((key) => {
      const row = make('div', 'profile-row');
      const icon = make('span', 'row-glyph');
      icon.append(glyph('key'));
      const copy = make('span', 'row-copy');
      copy.append(make('b', '', key.label), make('small', '', new Date(key.created).toLocaleDateString(language)));
      row.append(icon, copy);
      if (keys.length > 1) row.append(button(t('remove'), 'plain', () => sheet(t('removePasskey'),
        make('p', 'sheet-copy', t('removePasskeyConfirmation')),
        button(t('remove'), 'primary', async () => {
          closeSheet();
          try { await api('/api/passkeys', 'DELETE', { id: key.id }); loadKeys(); } catch (error) { toast(error.message); }
        }))));
      return row;
    }));
  } catch (error) { if (signedIn) toast(error.message); }
}

async function inviteDevice(event) {
  const trigger = event.currentTarget;
  trigger.disabled = true;
  try {
    const { url, expiresInMinutes } = await api('/api/passkeys/invitations', 'POST');
    if (!signedIn || route !== 'mine' || minePage !== 'keys') return;
    const panel = make('div', 'pane-form');
    const label = make('label', '', t('enrollmentLink'));
    const input = make('input');
    input.readOnly = true;
    input.autocomplete = 'off';
    input.value = url;
    label.append(input);
    const copy = button(t('copyLink'), 'primary', async () => {
      try {
        await navigator.clipboard.writeText(url);
        copy.textContent = t('linkCopied');
      } catch {
        input.focus();
        input.select();
      }
    });
    panel.append(label, copy);
    sheet(t('setupAnotherDevice'), make('p', 'sheet-copy', t('deviceInviteHelp', { count: expiresInMinutes })), panel);
  } catch (error) {
    if (signedIn) toast(error.message);
  } finally { trigger.disabled = false; }
}

async function authenticate(invite = null, label = null) {
  if (!passkeys?.browserSupportsWebAuthn()) throw new Error(t('passkeyBrowser'));
  const registering = Boolean(invite || label);
  const base = '/api/auth/' + (registering ? 'register' : 'login');
  const { options, ticket } = await api(base + '/options', 'POST', { invite, label });
  const response = registering
    ? await passkeys.startRegistration({ optionsJSON: options })
    : await passkeys.startAuthentication({ optionsJSON: options });
  await api(base + '/finish', 'POST', { ticket, response });
}

function registerSheet() {
  const form = make('form', 'pane-form');
  const label = make('label', '', t('deviceName'));
  const input = make('input');
  input.placeholder = t('deviceExample');
  input.maxLength = 64;
  input.required = true;
  label.append(input);
  const submit = make('button', 'primary', t('createPasskey'));
  submit.type = 'submit';
  form.append(label, submit);
  form.addEventListener('submit', async (event) => {
    event.preventDefault();
    submit.disabled = true;
    try {
      await authenticate(enrollment, input.value.trim() || t('passkeyLabel'));
      enrollment = null;
      closeSheet();
      if (!signedIn) signedOn();
      else loadKeys();
    } catch (error) { closeSheet(); (signedIn ? toast : (value) => status('login-error', value))(error.message); }
  });
  sheet(t('addPasskey'), form);
  input.focus();
}

function signedOn() {
  signedIn = true;
  route = 'agents';
  overview = '';
  scope = 'all';
  opened = false;
  paint();
  save('replaceState');
  refresh(true);
}

function signedOut() {
  signedIn = false;
  route = 'login';
  machines = [];
  overview = '';
  active = null;
  activeKey = '';
  transcript = null;
  selection.clear();
  drafts.clear();
  byId('messages').replaceChildren();
  byId('groups').replaceChildren();
  byId('node-list').replaceChildren();
  byId('usage-windows').replaceChildren();
  byId('key-list').replaceChildren();
  byId('detail-name').textContent = '';
  byId('detail-context').textContent = '';
  delete chatInput.dataset.thread;
  chatInput.value = '';
  termInput.value = '';
  closeSheet();
  paint();
  save('replaceState');
}

nav.forEach((item) => item.addEventListener('click', () => navigate(item.dataset.route)));
byId('machine').addEventListener('click', switchMachine);
byId('back').addEventListener('click', back);
byId('mine-back').addEventListener('click', back);
byId('chat-tab').addEventListener('click', () => tab('chat'));
byId('term-tab').addEventListener('click', () => tab('terminal'));
byId('older').addEventListener('click', () => loadHistory(true));
byId('chat-form').addEventListener('submit', (event) => { event.preventDefault(); send(); });
chatInput.addEventListener('input', () => drafts.set(activeKey, chatInput.value));
for (const trigger of document.querySelectorAll('[data-pane]')) trigger.addEventListener('click', paneSheet);
byId('control').addEventListener('click', () => sendTerminal({ type: terminal?.writable ? 'release' : 'take' }));
byId('term-form').addEventListener('submit', (event) => {
  event.preventDefault();
  if (!terminal?.writable) return;
  const text = termInput.value;
  const control = terminal.ctrl && /^[a-z]$/i.test(text);
  sendTerminal({ type: 'input', data: control ? String.fromCharCode(text.toUpperCase().charCodeAt(0) - 64) : text + '\r' });
  termInput.value = '';
  terminal.ctrl = false;
  terminalControls('inputEnabled');
});
for (const item of document.querySelectorAll('[data-key]')) item.addEventListener('click', () => {
  if (!terminal?.writable) return;
  const key = item.dataset.key;
  if (key === 'Ctrl') terminal.ctrl = !terminal.ctrl;
  else {
    sendTerminal({ type: 'input', data: { Esc: '\x1b', Tab: '\t', '↑': '\x1b[A', '↓': '\x1b[B', '←': '\x1b[D', '→': '\x1b[C' }[key] });
    terminal.ctrl = false;
  }
  terminalControls('inputEnabled');
});
byId('detail-tabs').addEventListener('keydown', (event) => {
  if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
  event.preventDefault();
  const next = event.key === 'Home' ? 'chat' : event.key === 'End' ? 'terminal' : view === 'chat' ? 'terminal' : 'chat';
  tab(next);
  focus(next === 'chat' ? 'chat-tab' : 'term-tab');
});
byId('close').addEventListener('click', closeSheet);
overlay.addEventListener('click', (event) => { if (event.target === overlay) closeSheet(); });
byId('mine-passkeys').addEventListener('click', () => openMine('keys'));
byId('mine-preferences').addEventListener('click', () => openMine('settings'));
byId('usage-refresh').addEventListener('click', () => loadUsage(true));
byId('add-key').addEventListener('click', registerSheet);
byId('invite-device').addEventListener('click', inviteDevice);
byId('passkey').addEventListener('click', async (event) => {
  event.currentTarget.disabled = true;
  status('login-error');
  try { await authenticate(); signedOn(); }
  catch (error) { status('login-error', error.message); }
  finally { byId('passkey').disabled = false; }
});
byId('enroll').addEventListener('click', () => {
  if (enrollment) registerSheet();
  else sheet(t('firstDevice'), make('p', 'sheet-copy', t('inviteInstructions')));
});
byId('signout').addEventListener('click', async () => {
  try { await api('/api/logout', 'POST'); signedOut(); } catch (error) { if (signedIn) toast(error.message); }
});
byId('install').addEventListener('click', async () => {
  if (installPrompt) { await installPrompt.prompt(); installPrompt = null; }
  else sheet(t('install'), make('p', 'sheet-copy', t(window.matchMedia('(display-mode: standalone)').matches
    ? 'alreadyInstalled' : 'androidInstallInstructions')));
});
window.addEventListener('beforeinstallprompt', (event) => { event.preventDefault(); installPrompt = event; });
window.addEventListener('hashchange', () => {
  const invite = readEnrollment();
  if (invite) {
    enrollment = invite;
    registerSheet();
  }
});
window.addEventListener('popstate', (event) => {
  if (!signedIn || !event.state?.workbench) return;
  closeSheet();
  const saved = event.state;
  route = saved.route === 'login' ? 'agents' : saved.route;
  scope = saved.scope;
  opened = saved.opened;
  minePage = saved.minePage;
  if (saved.selected) selection.set(route, saved.selected);
  paint();
  if (isWorkspace() && active && saved.view !== view) { view = saved.view; paintDetail(); }
  if (route === 'mine') { if (minePage === 'keys') loadKeys(); else loadUsage(); }
});
mobile.addEventListener('change', () => { if (isWorkspace()) paintDetail(); });
window.addEventListener('online', () => { if (signedIn) refresh(true); });
window.addEventListener('offline', () => toast(t('networkInterrupted')));

const viewport = window.visualViewport;
let width = window.innerWidth;
let fullHeight = viewport?.height ?? window.innerHeight;
function keyboard() {
  const editing = [chatInput, termInput, terminal?.xterm?.textarea].includes(document.activeElement);
  const height = viewport?.height ?? window.innerHeight;
  if (!editing || width !== window.innerWidth) { width = window.innerWidth; fullHeight = height; }
  const visible = mobile.matches && editing && (viewport?.scale ?? 1) < 1.05 && fullHeight - height > 125;
  app.toggleAttribute('data-ime', visible);
  if (visible) app.style.setProperty('--ime-height', height + 'px');
  else app.style.removeProperty('--ime-height');
}
window.addEventListener('resize', keyboard);
viewport?.addEventListener('resize', keyboard);
document.addEventListener('focusin', keyboard);
document.addEventListener('focusout', () => requestAnimationFrame(keyboard));
for (const [container, field] of [[byId('chat-form'), chatInput], [byId('term-form'), termInput], [byId('term').querySelector('.keys'), termInput]]) {
  container.addEventListener('pointerdown', (event) => {
    if (document.activeElement === field && event.target.closest('button')) event.preventDefault();
  });
}

if ('serviceWorker' in navigator) navigator.serviceWorker.register('/sw.js').catch(() => {});
api('/api/session').then(({ authenticated }) => {
  if (authenticated) signedOn();
  if (enrollment) registerSheet();
}).catch(() => status('login-error', connectionError));
setInterval(() => {
  if (document.hidden) return;
  refresh();
  if (signedIn && view === 'chat' && isWorkspace() && active?.structured && (opened || !mobile.matches)) loadHistory();
}, 3000);
