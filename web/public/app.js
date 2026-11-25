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
const passkeys = window.SimpleWebAuthnBrowser;
const loopback = location.protocol === 'http:' && location.hostname === 'localhost';
const connectionError = t(loopback ? 'sshTunnelUnavailable' : 'gatewayUnavailable');
const stateNames = { idle: t('idle'), blocked: t('blocked'), working: t('working'), done: t('done'), unknown: t('unknownStatus') };
const nodeStates = { pending: t('pending'), offline: t('offline'), online: t('online') };
const kindNames = { codex: 'Codex', claude: 'Claude Code', opencode: 'OpenCode', trae: 'Trae', traex: 'Trae', shell: t('terminal') };
const marks = { codex: 'Cx', claude: 'Cl', opencode: 'Oc', trae: 'Tr', shell: '>_' };
const labels = { agents: t('agents'), sessions: t('sessions'), machines: t('machines'), mine: t('profile') };
const states = ['blocked', 'done', 'working', 'idle'];

let signedIn = false;
let route = 'login';
let scope = 'all';
let opened = false;
let minePage = 'home';
let machines = [];
let overview = '';
let active = null;
let activeKey = '';
let toastTimer;
let refreshing = 0;
let syncing = false;
let launching = null;
let installPrompt;
let enrollment = readEnrollment();
const selection = new Map();
const selectedShell = new Map();
const hiddenStates = new Set();
const expandedTeams = new Set();
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
  delete overlay.dataset.position;
  byId('sheet-title').textContent = title;
  byId('sheet-body').replaceChildren(...contents);
  if (!overlay.open) overlay.showModal();
  byId('close').focus();
}

function closeSheet() {
  if (overlay.open) overlay.close();
}

function isWorkspace() { return route === 'agents' || route === 'sessions'; }
function recordKey(item) {
  if (!item) return '';
  const identity = item.sessionRecord ? item.pane?.key.split('.').slice(0, 2).join('.') : item.pane?.key;
  return [item.node, item.id, identity || ''].join('/');
}
function conversationKey() { return activeKey + '/' + (active?.thread || ''); }
function focus(id) { byId(id).focus({ preventScroll: true }); }

function save(method, up = false) {
  history[method]({ workbench: true, route, scope, opened, minePage, selected: activeKey, up }, '');
}

function navigate(target, node = scope) {
  if (target === route && node === scope) {
    if ((isWorkspace() && opened) || (route === 'mine' && minePage !== 'home')) back();
    return;
  }
  closeSheet();
  launching = null;
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
  if (isWorkspace()) paintWorkspace();
  else {
    stopTerminal();
    if (route === 'machines') paintMachines();
    if (route === 'mine') paintMine();
    paintHeader();
  }
}

function paintHeader() {
  byId('home-bar').hidden = !signedIn || (isWorkspace() && opened && mobile.matches)
    || (route === 'mine' && minePage !== 'home');
  byId('module-label').textContent = labels[route] || '';
  byId('machine').hidden = !isWorkspace();
  const machine = machines.find(({ id }) => id === scope);
  byId('scope').textContent = machine?.name || t('allMachines');
  byId('health').textContent = machine ? nodeStates[machine.state] : t('onlineCount', {
    count: machines.filter(({ state }) => state === 'online').length,
  });
}

function scopedMachines() {
  return scope === 'all' ? machines : machines.filter(({ id }) => id === scope);
}

function withMachine(item, machine) {
  return { ...item, node: machine.id, host: machine.name };
}

function records(machine) {
  if (route === 'sessions') {
    const sessions = new Map((machine.sessions || []).map((item) => [item.id, withMachine({ ...item, sessionRecord: true }, machine)]));
    const ids = machine.sidebar?.sessionRows || [...sessions.keys()];
    return {
      rows: ids.map((id) => sessions.get(id)).filter(Boolean),
      pinned: (machine.sidebar?.pinnedSessions || []).map((id) => sessions.get(id)).filter(Boolean),
    };
  }
  const agents = new Map((machine.agents || []).map((item) => [item.id, withMachine(item, machine)]));
  const rows = [];
  let teamOpen = false;
  for (const item of machine.sidebar?.agentRows || [...agents.values()].filter(({ state }) => !hiddenStates.has(state))) {
    if (item.group !== undefined) { rows.push(item); continue; }
    if (item.team) teamOpen = expandedTeams.has(machine.id + '/' + item.id);
    if (agents.has(item.id) && (!item.worker || teamOpen)) rows.push({ ...agents.get(item.id), ...item });
  }
  return { rows, pinned: (machine.pinnedAgents || []).map((item) => withMachine(item, machine)) };
}

function paintWorkspace() {
  const rows = scopedMachines().flatMap((machine) => {
    const { rows, pinned } = records(machine);
    return [...rows.filter((item) => item.id), ...pinned];
  });
  const previous = conversationKey();
  active = rows.find((item) => recordKey(item) === selection.get(route)) || rows[0] || null;
  activeKey = recordKey(active);
  if (previous !== conversationKey()) {
    transcript = null;
    status('chat-error');
    stopTerminal();
  }
  if (!active || (opened && selection.has(route) && selection.get(route) !== activeKey)) opened = false;
  byId('workspace').dataset.mobileView = opened ? 'detail' : 'list';
  paintHeader();
  paintCollection();
  paintDetail();
}

function stateDot(state, colors) {
  const dot = make('span', 'state-dot');
  const color = colors?.[state];
  if (/^#[0-9a-f]{6}$/i.test(color || '')) dot.style.setProperty('--state-color', color);
  dot.setAttribute('aria-hidden', 'true');
  return dot;
}

function paintStateFilters() {
  const filters = byId('state-filters');
  filters.hidden = route !== 'agents';
  if (filters.hidden) return;
  const nodes = scopedMachines().filter((machine) => machine.snapshotAvailable);
  const activeFilter = document.activeElement?.dataset.filter;
  filters.replaceChildren(...states.map((state) => {
    const count = nodes.reduce((sum, machine) => sum + (machine.sidebar?.stats?.find((item) => item.state === state)?.count
      ?? machine.agents?.filter((agent) => agent.state === state).length ?? 0), 0);
    const chip = button('', 'filter-chip', () => {
      if (hiddenStates.has(state)) hiddenStates.delete(state);
      else hiddenStates.add(state);
      paintStateFilters();
      refresh(true);
    });
    chip.dataset.filter = state;
    chip.title = stateNames[state];
    chip.setAttribute('aria-pressed', String(!hiddenStates.has(state)));
    chip.setAttribute('aria-label', stateNames[state] + ', ' + count);
    chip.append(stateDot(state, nodes.find((node) => node.sidebar)?.sidebar.colors),
      make('span', 'filter-label', stateNames[state]), make('b', '', String(count)));
    if (state === activeFilter) requestAnimationFrame(() => chip.focus({ preventScroll: true }));
    return chip;
  }));
}

function recordRow(item, machine, pinned, focusKey) {
  const session = route === 'sessions';
  const container = make('div', 'record-row' + (item.worker ? ' worker-row' : ''));
  const row = button('', 'agent-row', () => openRecord(item));
  row.dataset.record = recordKey(item);
  if (row.dataset.record === activeKey && (opened || !mobile.matches)) row.setAttribute('aria-current', 'true');
  const icon = make('span', 'agent-tag ' + (item.kind || 'session'), session ? undefined : marks[item.kind] || '>_');
  if (session) icon.append(glyph('sessions'));
  icon.setAttribute('aria-hidden', 'true');
  const copy = make('span', 'record-copy');
  const title = item.worker ? item.worker.name : item.title;
  const subtype = session ? [t('agentCount', { count: item.agentCount }), item.branch].filter(Boolean).join(' · ')
    : [item.worker?.title || kindNames[item.kind], machine.sidebar?.sort === 'grouped' ? '' : item.session].filter(Boolean).join(' · ');
  copy.append(make('b', '', title), make('span', 'agent-meta', subtype));
  if (item.exited) copy.append(make('span', 'agent-meta', t('agentExited')));
  const badge = make('span', 'record-state');
  badge.title = stateNames[item.state] || '';
  badge.append(stateDot(item.state, machine.sidebar?.colors), make('span', 'sr-only', badge.title));
  row.append(icon, copy, badge);
  container.append(row);
  if (row.dataset.record === focusKey) requestAnimationFrame(() => row.focus({ preventScroll: true }));
  if (item.team) {
    const key = machine.id + '/' + item.id;
    const team = button(String(item.team.workers), 'row-action team-toggle', () => {
      if (expandedTeams.has(key)) expandedTeams.delete(key);
      else expandedTeams.add(key);
      paintWorkspace();
    });
    team.prepend(glyph('down'));
    team.setAttribute('aria-expanded', String(expandedTeams.has(key)));
    team.setAttribute('aria-label', t('teamWorkers', { count: item.team.workers, blocked: item.team.blocked, done: item.team.done }));
    container.append(team);
  }
  if (machine.sidebar && item.pane && !item.worker) {
    const target = session ? item.id : item.pane.window;
    const assistant = !session && machine.pinnedAgents?.some((card) => card.pane?.window === target && card.assistant);
    const pin = button('', 'row-action pin-button', async () => {
      pin.disabled = true;
      try {
        await api('/api/pins', 'POST', { node: machine.id, scope: session ? 'session' : 'agent', target, pane: item.pane.key, pinned: !pinned });
        await refresh(true);
      } catch (error) { toast(error.message); }
      finally { pin.disabled = Boolean(assistant); }
    });
    pin.append(glyph('pin'));
    pin.disabled = Boolean(assistant);
    pin.title = t(assistant ? 'assistantPinned' : pinned ? 'unpin' : 'pin');
    pin.setAttribute('aria-label', pin.title);
    pin.setAttribute('aria-pressed', String(pinned));
    container.append(pin);
  }
  return container;
}

function paintCollection() {
  byId('list-heading').textContent = labels[route];
  byId('new-agent').disabled = !machines.some((machine) => machine.sidebar?.launchTool && machine.sessions?.some((session) => session.pane));
  paintStateFilters();
  const groups = byId('groups');
  const focusKey = document.activeElement?.dataset.record;
  groups.replaceChildren();

  for (const machine of scopedMachines()) {
    const group = make('section', 'group');
    const { rows, pinned } = records(machine);
    const heading = make('h3');
    const count = machine.state === 'online' && machine.snapshotAvailable
      ? t(route === 'agents' ? 'agentCount' : 'sessionCount', {
        count: (route === 'agents' ? machine.agents : machine.sessions)?.length || 0,
      }) : nodeStates[machine.state];
    heading.append(make('span', machine.state === 'online' ? 'dot' : 'dot off'), make('span', 'group-name', machine.name), make('span', 'count', count));
    if (machine.sidebar) {
      const sort = make('select', 'sort-select');
      sort.setAttribute('aria-label', t('sortOrder', { machine: machine.name }));
      sort.add(new Option(t(route === 'agents' ? 'groupBySession' : 'sortByName'), 'grouped'));
      sort.add(new Option(t('prioritized'), 'prioritized'));
      sort.value = machine.sidebar.sort;
      sort.addEventListener('change', async () => {
        sort.disabled = true;
        try {
          await api('/api/sidebar-sort', 'POST', { node: machine.id, sort: sort.value });
          await refresh(true);
        } catch (error) { sort.value = machine.sidebar.sort; toast(error.message); }
        finally { sort.disabled = false; }
      });
      heading.append(sort);
    }
    group.append(heading);
    const pinnedIds = new Set(pinned.map((item) => route === 'sessions' ? item.id : item.pane?.window));
    if (pinned.length) {
      const rack = make('div', 'pinned-records');
      rack.append(make('h4', 'subheading', t('pinned')), ...pinned.map((item) => recordRow(item, machine, true, focusKey)));
      group.append(rack);
    }
    if (!rows.length && !pinned.length) {
      const message = machine.state === 'pending' ? 'waitingForPairing' : machine.state === 'offline' ? 'nodeReconnect'
        : !machine.snapshotAvailable ? 'snapshotUnavailable' : route === 'sessions' ? 'noSessions'
          : machine.agents?.length ? 'noMatchingAgents' : 'noAgents';
      group.append(make('div', 'empty', t(message)));
    }
    group.append(...rows.map((item) => item.group !== undefined ? make('h4', 'subheading', item.group)
      : recordRow(item, machine, pinnedIds.has(route === 'sessions' ? item.id : item.pane?.window), focusKey)));
    groups.append(group);
  }
  if (!machines.length) groups.append(make('div', 'empty', t('loadingWorkspace')));
}

function openRecord(item) {
  const fromList = !opened;
  launching = null;
  opened = true;
  selection.set(route, recordKey(item));
  paintWorkspace();
  save('pushState', fromList);
  if (mobile.matches) focus('detail-name');
}

function paintDetail() {
  byId('detail').hidden = false;
  byId('detail').querySelector('.detail-bar').hidden = !active;
  byId('new-shell').hidden = route !== 'sessions' || !opened || !active?.pane;
  const empty = byId('detail-empty');
  byId('chat').hidden = !(route === 'agents' && active?.structured);
  if (route !== 'sessions' || !active) byId('term').hidden = true;
  if (!active) {
    empty.hidden = false;
    empty.textContent = t(route === 'sessions' ? 'selectSession' : 'chooseMachine');
    return;
  }
  byId('detail-name').textContent = active.title;
  byId('detail-context').textContent = [active.host, active.session || active.branch || kindNames[active.kind]].filter(Boolean).join(' / ');
  byId('back').setAttribute('aria-label', t(route === 'agents' ? 'backToAgents' : 'backToSessions'));
  if (route === 'sessions') { paintShells(empty); return; }
  stopTerminal();
  empty.hidden = Boolean(active.structured);
  if (!active.structured) { empty.textContent = t('noReadableHistory'); return; }
  if (!opened && mobile.matches) return;
  paintComposer();
  const key = conversationKey();
  if (transcript?.key !== key) {
    transcript = { key, rows: new Map(), before: null, loaded: false, busy: false };
    byId('messages').replaceChildren(make('p', 'day', t('readingHistory')));
    byId('older').hidden = true;
    loadHistory();
  }
}

function sessionShells() {
  return (machines.find(({ id }) => id === active?.node)?.terminals || [])
    .filter((item) => item.kind === 'shell' && item.pane.owned && item.pane.session === active.id);
}

function paintShells(empty) {
  const shells = opened ? sessionShells() : [];
  const selected = selectedShell.get(active.node + '/' + active.id);
  const pane = (shells.find((item) => item.pane.key === selected) || shells[0])?.pane;
  empty.hidden = Boolean(pane);
  byId('term').hidden = !pane;
  if (!pane) {
    stopTerminal();
    empty.textContent = t(!opened ? 'selectSession' : active.pane ? 'noShells' : 'noSessionPane');
    return;
  }
  const picker = byId('shells');
  const keys = shells.map((item) => item.pane.key).join('/');
  if (picker.dataset.keys !== keys) {
    picker.replaceChildren(...shells.map(({ pane }) => new Option(t('terminalPane', { pane: pane.pane }), pane.key)));
    picker.dataset.keys = keys;
  }
  picker.value = pane.key;
  startTerminal(pane);
}

function paintComposer() {
  const key = conversationKey();
  if (chatInput.dataset.thread !== key) {
    chatInput.dataset.thread = key;
    chatInput.value = drafts.get(key) || '';
  }
  const ready = ['idle', 'done'].includes(active.state) && !active.pane.inMode;
  byId('send').disabled = !ready || submitting;
  byId('chat-status').textContent = t(ready ? 'originalConversation' : active.state === 'blocked' ? 'agentBlocked' : 'agentWorking');
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
  const key = conversationKey();
  const text = chatInput.value;
  submitting = true;
  paintComposer();
  status('chat-error');
  try {
    await api('/api/prompt', 'POST', { node: item.node, pane: item.pane.key, agent: item.id, thread: item.thread, text });
    if (drafts.get(key) === text) drafts.delete(key);
    if (conversationKey() === key) {
      if (chatInput.value === text) chatInput.value = '';
      loadHistory();
    }
  } catch (error) {
    if (conversationKey() === key) status('chat-error', error.message);
  } finally {
    submitting = false;
    if (active?.structured && route === 'agents') paintComposer();
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

function startTerminal(pane) {
  const key = active.node + '/' + pane.key;
  if (terminal?.key === key) return;
  stopTerminal();
  const current = { key, node: active.node, pane, writable: false, ready: false, ctrl: false, alt: false };
  terminal = current;
  termInput.value = '';
  const renderer = new window.Terminal({
    cols: Math.max(1, pane.cols), rows: Math.max(1, pane.rows), fontSize: 13,
    cursorBlink: false, scrollback: 2000, theme: { background: '#17243a', foreground: '#e2edff', cursor: '#b3c6ff' },
    disableStdin: true, cursorInactiveStyle: 'outline',
  });
  current.xterm = renderer;
  current.fit = new window.FitAddon.FitAddon();
  renderer.loadAddon(current.fit);
  renderer.open(byId('term-screen'));
  renderer.onData((data) => sendKeys(data));
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
  current.alt = false;
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
  byId('control').setAttribute('aria-pressed', String(writable));
  byId('term-form').hidden = !writable;
  for (const item of document.querySelectorAll('[data-key]')) item.disabled = !writable;
  document.querySelector('[data-key="Ctrl"]').setAttribute('aria-pressed', String(Boolean(terminal?.ctrl && writable)));
  document.querySelector('[data-key="Alt"]').setAttribute('aria-pressed', String(Boolean(terminal?.alt && writable)));
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

function sendKeys(data, submit = false) {
  if (!terminal?.writable) return;
  const control = terminal.ctrl && /^[a-z]$/i.test(data);
  const alt = terminal.alt;
  if (control) data = String.fromCharCode(data.toUpperCase().charCodeAt(0) - 64);
  if (alt) data = '\x1b' + data;
  if (submit && !control && !alt) data += '\r';
  sendTerminal({ type: 'input', data });
  terminal.ctrl = terminal.alt = false;
  terminalControls('inputEnabled');
}

function choice(label, detail, action) {
  const row = button('', 'sheet-row', action);
  row.append(make('span', 'choice', label), make('small', '', detail));
  return row;
}

function switchModule() {
  const rows = Object.entries(labels).map(([name, label]) => {
    const row = choice(label, '', () => { closeSheet(); navigate(name); });
    row.prepend(glyph(name));
    row.setAttribute('aria-current', String(name === route));
    if (name === route) row.append(glyph('next'));
    return row;
  });
  sheet(t('mainNavigation'), ...rows);
  overlay.dataset.position = 'start';
}

function switchMachine() {
  const all = { id: 'all', name: t('allMachines') };
  sheet(t('switchMachine'), ...[all, ...machines].map((item) => {
    const row = choice(item.name, nodeStates[item.state] || t('machineCount', { count: machines.length }), () => {
      scope = item.id;
      opened = false;
      launching = null;
      closeSheet();
      paintWorkspace();
      save('replaceState');
    });
    row.setAttribute('aria-current', String(scope === item.id));
    if (item.state) row.prepend(make('span', item.state === 'online' ? 'dot' : 'dot off'));
    return row;
  }));
  overlay.dataset.position = 'end';
}

function newAgentSheet() {
  const online = machines.filter((item) => item.state === 'online');
  const form = make('form', 'workspace-form');
  const machine = make('select');
  const session = make('select');
  const summary = make('p', 'sheet-note');
  const submit = make('button', 'primary', t('newAgent'));
  submit.type = 'submit';
  for (const item of online) machine.add(new Option(item.name, item.id));
  const preferred = scope === 'all' ? active?.node : scope;
  const eligible = online.find((item) => item.sidebar?.launchTool && item.sessions?.some((entry) => entry.pane));
  if (online.some(({ id }) => id === preferred)) machine.value = preferred;
  else if (eligible) machine.value = eligible.id;
  const sessionLabel = make('label', '', t('tmuxSession'));
  sessionLabel.append(session);
  const machineLabel = make('label', '', t('machine'));
  machineLabel.append(machine);
  form.append(machineLabel, sessionLabel, summary, submit);
  function locations() {
    const node = online.find(({ id }) => id === machine.value);
    const sessions = (node?.sessions || []).filter((item) => item.pane);
    session.replaceChildren(...sessions.map((item) => new Option(item.title, item.id)));
    if (machine.value === active?.node && sessions.some(({ id }) => id === active.pane?.session)) session.value = active.pane.session;
    const tool = node?.sidebar?.launchTool;
    summary.textContent = !node ? t('noOnlineMachines') : !tool ? t('noAgentLauncher')
      : !sessions.length ? t('noSessions') : t('launchUsing', { tool: kindNames[tool] || tool });
    session.disabled = !sessions.length;
    submit.disabled = !tool || !sessions.length;
  }
  locations();
  machine.addEventListener('change', locations);
  form.addEventListener('submit', async (event) => {
    event.preventDefault();
    const node = online.find(({ id }) => id === machine.value);
    const target = node?.sessions?.find(({ id }) => id === session.value);
    if (submit.disabled || !target?.pane) return;
    machine.disabled = session.disabled = true;
    submit.disabled = true;
    try {
      const result = await api('/api/agents', 'POST', { node: node.id, session: target.id, pane: target.pane.key });
      closeSheet();
      if (route === 'agents' && scope === node.id) {
        opened = false;
        paintWorkspace();
        save('replaceState');
      } else navigate('agents', node.id);
      if (result.pane) launching = { node: node.id, pane: result.pane.key, expires: Date.now() + 15_000 };
      toast(t('agentStarted'));
      await refresh(true);
    } catch (error) { summary.textContent = error.message; }
    finally { machine.disabled = session.disabled = submit.disabled = false; }
  });
  sheet(t('newAgent'), form);
}

function shellSheet() {
  const item = active;
  if (route !== 'sessions' || !item?.pane) return;
  const selected = byId('shells').value;
  const anchor = sessionShells().find(({ pane }) => pane.key === selected)?.pane || item.pane;
  const form = make('form', 'workspace-form');
  const placement = make('select');
  placement.add(new Option(t('newWindow'), 'window'));
  placement.add(new Option(t('splitPane'), 'split'));
  const label = make('label', '', t('placement'));
  label.append(placement);
  const submit = make('button', 'primary', t('newTerminal'));
  submit.type = 'submit';
  const summary = make('p', 'sheet-note', item.host + ' / ' + item.title);
  form.append(label, summary, submit);
  form.addEventListener('submit', async (event) => {
    event.preventDefault();
    if (submit.disabled) return;
    submit.disabled = true;
    try {
      const pane = await api('/api/panes', 'POST', { node: item.node, session: item.id, anchor: anchor.key, split: placement.value === 'split' });
      selectedShell.set(item.node + '/' + item.id, pane.key);
      closeSheet();
      await refresh(true);
    } catch (error) { summary.textContent = error.message; }
    finally { submit.disabled = false; }
  });
  sheet(t('newTerminal'), form);
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
    if (item.state === 'online') {
      copy.append(make('small', '', item.snapshotAvailable ? t('agentCount', { count: item.agents?.length || 0 }) : t('snapshotUnavailable')));
    }
    const trailing = make('span', 'trailing');
    trailing.append(make('span', item.state === 'online' ? 'dot' : 'dot off'), nodeStates[item.state], glyph('next'));
    row.append(icon, copy, trailing);
    return row;
  }));
}

async function refresh(force = false) {
  if (!signedIn || (syncing && !force)) return;
  const revision = ++refreshing;
  syncing = true;
  try {
    const query = new URLSearchParams([...hiddenStates].map((state) => ['hide', state]));
    const result = await api('/api/workspace' + (query.size ? '?' + query : ''));
    if (!signedIn || revision !== refreshing) return;
    const fingerprint = JSON.stringify(result);
    if (fingerprint !== overview || force) {
      machines = result.nodes;
      overview = fingerprint;
      if (isWorkspace()) paintWorkspace();
      else if (route === 'machines') paintMachines();
    }
    if (launching?.expires < Date.now()) launching = null;
    if (launching && route === 'agents') {
      const machine = machines.find(({ id }) => id === launching.node);
      const agent = machine?.agents?.find(({ pane }) => pane.key === launching.pane);
      if (agent && records(machine).rows.some(({ id }) => id === agent.id)) openRecord(withMachine(agent, machine));
    }
  } catch (error) {
    if (signedIn && force) toast(error.message);
  } finally { if (revision === refreshing) syncing = false; }
}

function paintMine() {
  for (const section of ['home', 'keys', 'settings']) byId('mine-' + section).hidden = minePage !== section;
  byId('mine-back').hidden = minePage === 'home';
  byId('mine-heading').textContent = t({ home: 'profile', keys: 'passkeys', settings: 'settings' }[minePage]);
}

function openMine(page) {
  minePage = page;
  paintMine();
  paintHeader();
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
  selectedShell.clear();
  hiddenStates.clear();
  expandedTeams.clear();
  launching = null;
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

byId('modules').addEventListener('click', switchModule);
byId('machine').addEventListener('click', switchMachine);
byId('new-agent').addEventListener('click', newAgentSheet);
byId('new-shell').addEventListener('click', shellSheet);
byId('back').addEventListener('click', back);
byId('mine-back').addEventListener('click', back);
byId('shells').addEventListener('change', (event) => {
  if (!active || route !== 'sessions') return;
  selectedShell.set(active.node + '/' + active.id, event.target.value);
  paintDetail();
});
byId('older').addEventListener('click', () => loadHistory(true));
byId('chat-form').addEventListener('submit', (event) => { event.preventDefault(); send(); });
chatInput.addEventListener('input', () => drafts.set(conversationKey(), chatInput.value));
byId('control').addEventListener('click', () => sendTerminal({ type: terminal?.writable ? 'release' : 'take' }));
byId('term-form').addEventListener('submit', (event) => {
  event.preventDefault();
  if (!terminal?.writable) return;
  sendKeys(termInput.value, true);
  termInput.value = '';
});
const terminalKeys = {
  Esc: '\x1b', Tab: '\t', '↑': '\x1b[A', '↓': '\x1b[B', '←': '\x1b[D', '→': '\x1b[C',
  Home: '\x1b[H', End: '\x1b[F', PageUp: '\x1b[5~', PageDown: '\x1b[6~',
};
for (const item of document.querySelectorAll('[data-key]')) item.addEventListener('click', () => {
  if (!terminal?.writable) return;
  const key = item.dataset.key;
  if (key === 'Ctrl') terminal.ctrl = !terminal.ctrl;
  else if (key === 'Alt') terminal.alt = !terminal.alt;
  else sendKeys(terminalKeys[key]);
  terminalControls('inputEnabled');
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
  if (route === 'mine') { if (minePage === 'keys') loadKeys(); else loadUsage(); }
});
mobile.addEventListener('change', () => { if (isWorkspace()) paintWorkspace(); });
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
    const typed = document.activeElement === field || (container.classList.contains('keys') && document.activeElement === terminal?.xterm?.textarea);
    if (typed && event.target.closest('button')) event.preventDefault();
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
  if (signedIn && route === 'agents' && active?.structured && (opened || !mobile.matches)) loadHistory();
}, 3000);
