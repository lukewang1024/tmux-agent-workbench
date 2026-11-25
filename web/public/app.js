'use strict';

const { language, messages } = WORKBENCH_LOCALE;
const autoSizeComposer = CSS.supports('field-sizing', 'content');
const appVersion = 'WORKBENCH_CACHE_VERSION';
const plurals = new Intl.PluralRules(language);
const percentage = new Intl.NumberFormat(language, { style: 'percent' });
const byId = (id) => document.getElementById(id);
const app = byId('app');
const overlay = byId('overlay');
const pickerDialog = byId('picker-dialog');
const mobile = window.matchMedia('(max-width: 767px)');
const fullScreenPreview = window.matchMedia('(max-width: 1023px)');
const darkMode = window.matchMedia('(prefers-color-scheme: dark)');
const chatInput = byId('chat-input');
const termInput = byId('term-input');
const markdown = window.markdownit({ html: false, breaks: true, linkify: true });
const passkeys = window.SimpleWebAuthnBrowser;
const loopback = location.protocol === 'http:' && location.hostname === 'localhost';
const connectionError = t(loopback ? 'sshTunnelUnavailable' : 'gatewayUnavailable');
const stateNames = { idle: t('idle'), blocked: t('blocked'), working: t('working'), done: t('done'), unknown: t('unknownStatus') };
const nodeStates = { pending: t('pending'), offline: t('offline'), online: t('online') };
const kindNames = { codex: 'Codex', claude: 'Claude Code', opencode: 'OpenCode', trae: 'Trae', traex: 'Trae', shell: t('terminal') };
const providerMarks = { codex: 'chatgpt.svg', claude: 'claude.svg', opencode: 'opencode.svg', trae: 'trae.png' };
const labels = { agents: t('agents'), sessions: t('sessions'), machines: t('machines'), mine: t('profile') };
const states = ['blocked', 'done', 'working', 'idle'];
const stateRoles = { blocked: 'alert', done: 'info', working: 'warning', idle: 'accent', unknown: 'muted' };
const stateAnsi = { blocked: 1, done: 4, working: 3, idle: 2 };
const ansiNames = ['black', 'red', 'green', 'yellow', 'blue', 'magenta', 'cyan', 'white',
  'brightBlack', 'brightRed', 'brightGreen', 'brightYellow', 'brightBlue', 'brightMagenta', 'brightCyan', 'brightWhite'];

let signedIn = false;
let reauthRequired = false;
let authGeneration = 0;
let route = 'login';
let scope = 'all';
let opened = false;
let minePage = 'home';
let machines = [];
let machinePaletteIndexes = new Map();
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
const selectedPane = new Map();
const hiddenStates = new Set();
const expandedTeams = new Set();
const expandedSessions = new Set();
const drafts = new Map();
const outgoing = new Map();
const chatModels = new Map();
const chatPermissions = new Map();
const nextSettings = new Map();
const suggestionCache = new Map();
let suggestionIndex = 0;
let suggestionItems = [];
let composingPrompt = false;
let transcript = null;
let submitting = false;
let stopping = null;
let modelEditor = null;
let terminal = null;
let suppressHoldClick = false;
const linkTabs = new Map();
let activeLink = '';
let linkOpener;
let nextLinkId = 0;

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
  if (reauthRequired && !path.startsWith('/api/auth/') && path !== '/api/logout') {
    throw Object.assign(new Error(t('continueWithPasskey')), { status: 401 });
  }
  const generation = authGeneration;
  const response = await fetch(path, {
    method,
    headers: method === 'GET' ? undefined : { 'content-type': 'application/json' },
    body: method === 'GET' ? undefined : JSON.stringify(payload || {}),
    cache: 'no-store',
    signal: AbortSignal.timeout(path === '/api/usage' ? 27_000 : 13_000),
  }).catch(() => { throw new Error(connectionError); });
  const data = await response.json();
  if (!response.ok) {
    if (response.status === 401 && generation === authGeneration) requirePasskey();
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
  if (pickerDialog.open) pickerDialog.close();
  if (overlay.open) overlay.close();
}

function isWorkspace() { return route === 'agents' || route === 'sessions'; }
function recordKey(item) {
  if (!item) return '';
  const identity = item.sessionRecord ? item.pane?.key.split('.').slice(0, 2).join('.') : item.pane?.key;
  return [item.node, item.id, identity || ''].join('/');
}
function supportsChat(item = active) { return Boolean(item?.chat ?? item?.structured); }

function conversationKey(item = active) {
  return item?.thread ? item.node + '/thread/' + item.thread : recordKey(item);
}

const draftStorage = 'workbench-chat-v1/draft/';
const deliveryStorage = 'workbench-chat-v1/delivery/';
const settingsStorage = 'workbench-chat-v1/settings/';

function cacheChat(key, value) {
  try {
    if (value) localStorage.setItem(key, JSON.stringify(value));
    else localStorage.removeItem(key);
  } catch {}
}

function rememberDraft(key, text, retry) {
  if (text) drafts.set(key, { text, retry });
  else drafts.delete(key);
  cacheChat(draftStorage + key, drafts.get(key));
}

function rememberSettings(key, settings) {
  if (settings.model || settings.permission) nextSettings.set(key, settings);
  else nextSettings.delete(key);
  cacheChat(settingsStorage + key, nextSettings.get(key));
}

function restoreChats() {
  try {
    for (const storageKey of Object.keys(localStorage)) {
      const isDraft = storageKey.startsWith(draftStorage);
      const isSettings = storageKey.startsWith(settingsStorage);
      if (!isDraft && !isSettings && !storageKey.startsWith(deliveryStorage)) continue;
      let saved;
      try { saved = JSON.parse(localStorage.getItem(storageKey)); } catch { continue; }
      if (isSettings) {
        if (saved?.model || saved?.permission) nextSettings.set(storageKey.slice(settingsStorage.length), saved);
        continue;
      }
      if (typeof saved?.text !== 'string' || !saved.text) continue;
      if (isDraft) {
        drafts.set(storageKey.slice(draftStorage.length), saved);
        continue;
      }
      if (storageKey !== deliveryStorage + saved.id || typeof saved.key !== 'string'
          || !['sending', 'sent', 'failed'].includes(saved.state)) continue;
      if (saved.state === 'sending') {
        saved.state = 'failed';
        saved.error = t('sendUnconfirmed');
      }
      const queue = outgoing.get(saved.key) || { entries: [], matched: new Set() };
      queue.entries.push(saved);
      outgoing.set(saved.key, queue);
    }
    for (const [key, queue] of outgoing) {
      queue.entries.sort((a, b) => a.startedAt - b.startedAt);
      const failed = queue.entries.findLast((entry) => entry.state === 'failed');
      if (failed && !drafts.has(key)) drafts.set(key, { text: failed.text, retry: failed.id });
    }
  } catch {}
}

function promoteChats(nodes) {
  for (const machine of nodes) {
    for (const agent of machine.agents || []) {
      if (!agent.thread) continue;
      const item = withMachine(agent, machine);
      const previous = conversationKey({ ...item, thread: null });
      const key = conversationKey(item);
      const draft = drafts.get(previous);
      const queue = outgoing.get(previous);
      if (chatModels.has(previous)) {
        chatModels.set(key, chatModels.get(previous));
        chatModels.delete(previous);
      }
      if (chatPermissions.has(previous)) {
        chatPermissions.set(key, chatPermissions.get(previous));
        chatPermissions.delete(previous);
      }
      if (nextSettings.has(previous)) {
        const pending = nextSettings.get(previous);
        rememberSettings(key, nextSettings.has(key) ? { ...pending, ...nextSettings.get(key) } : pending);
        rememberSettings(previous, {});
      }
      if (!draft && !queue) continue;
      if (draft && !drafts.has(key)) rememberDraft(key, draft.text, draft.retry);
      if (queue) {
        const current = outgoing.get(key);
        for (const entry of queue.entries) {
          entry.key = key;
          cacheChat(deliveryStorage + entry.id, entry);
        }
        if (current) {
          current.entries.push(...queue.entries);
          current.entries.sort((a, b) => a.startedAt - b.startedAt);
          for (const id of queue.matched) current.matched.add(id);
        } else outgoing.set(key, queue);
      }
      if (draft) rememberDraft(previous, '');
      outgoing.delete(previous);
    }
  }
}
function focus(id) { byId(id).focus({ preventScroll: true }); }

function historyView(up) {
  return { workbench: true, route, scope, opened, minePage, selected: activeKey, up };
}

function save(method, up = false) {
  if (history.state?.messageSelection || history.state?.linkPreview) {
    clearMessageSelection();
    activeLink = '';
    paintLinkPanel();
    method = 'replaceState';
  }
  history[method](historyView(up), '');
}

function pushChatLayer(layer) {
  const view = { ...history.state, ...historyView(Boolean(history.state?.up)) };
  history.replaceState(view, '');
  history.pushState({ ...view, ...layer }, '');
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
  if (exitMessageSelection()) return;
  if (route === 'agents' && activeLink) { closeLinkPanel(); return; }
  if (history.state?.linkPreview) { history.back(); return; }
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
    paintPalette();
    stopTerminal();
    if (route === 'machines') paintMachines();
    if (route === 'mine') paintMine();
    paintHeader();
  }
  paintLinkPanel();
}

function paintHeader() {
  byId('home-bar').hidden = !signedIn || (isWorkspace() && opened && mobile.matches)
    || (route === 'mine' && minePage !== 'home');
  byId('module-label').textContent = labels[route] || '';
  byId('workspace-actions').hidden = !isWorkspace();
  const machine = machines.find(({ id }) => id === scope);
  const scopeName = machine?.name || t('allMachines');
  byId('scope').textContent = machine?.name || t('allShort');
  byId('machine').title = scopeName;
  byId('machine').setAttribute('aria-label', t('switchMachine') + ' · ' + scopeName);
}

function scopedMachines() {
  return scope === 'all' ? machines : machines.filter(({ id }) => id === scope);
}

function appearance() { return darkMode.matches ? 'dark' : 'light'; }

function themeColor(value) {
  if (typeof value === 'string' && /^#[0-9a-f]{6}$/i.test(value)) return value;
  if (!Number.isInteger(value) || value < 0 || value > 255) return '';
  if (value < 16) return 'var(--terminal-ansi-' + value + ')';
  const cube = [0, 95, 135, 175, 215, 255];
  const index = value - 16;
  const gray = 8 + (value - 232) * 10;
  const channels = value < 232 ? [cube[Math.floor(index / 36)], cube[Math.floor(index / 6) % 6], cube[index % 6]]
    : [gray, gray, gray];
  return '#' + channels.map((channel) => channel.toString(16).padStart(2, '0')).join('');
}

function stateColor(state, machine) {
  const palette = machine?.palette;
  if (!palette) return themeColor(machine?.sidebar?.colors?.[state]);
  const role = palette.roles?.[stateRoles[state]];
  if (palette.appearance === appearance() || Number.isInteger(role)) {
    const color = themeColor(role);
    if (color) return color;
  }
  return palette.full ? themeColor(stateAnsi[state]) : '';
}

function paintPalette() {
  const mode = appearance();
  const machine = machines.find(({ id }) => id === (scope === 'all' ? 'local' : scope)) || machines[0];
  const palette = machine?.palette;
  const roles = (palette?.appearance === mode && palette.roles) || {};
  const colors = {
    paper: themeColor(roles.background),
    ink: themeColor(roles.foreground),
    active: themeColor(roles.selection),
    muted: themeColor(roles.muted),
    raised: themeColor(roles.raised),
  };
  for (const state of states) colors['state-' + state] = stateColor(state, palette ? machine : null);
  machinePaletteIndexes = new Map(machines.map(({ id }, index) => [id, index]));
  for (const node of machines) {
    const prefix = 'machine-' + machinePaletteIndexes.get(node.id) + '-';
    for (const state of Object.keys(stateRoles)) colors[prefix + state] = stateColor(state, node);
  }
  const css = ':root { ' + Object.entries(colors).filter(([, color]) => color)
    .map(([role, color]) => '--' + role + ': ' + color + ';').join(' ') + ' }';
  const overrides = byId('palette-overrides');
  if (overrides.textContent !== css) overrides.textContent = css;
  const tab = document.querySelector('meta[name="theme-color"][media="(prefers-color-scheme: ' + mode + ')"]');
  tab.content = getComputedStyle(document.documentElement).getPropertyValue('--paper').trim();
}

function stateToken(state, machine) {
  const fallback = states.includes(state) ? 'var(--state-' + state + ')' : 'var(--muted)';
  const index = machinePaletteIndexes.get(machine?.id);
  if (index === undefined || !Object.hasOwn(stateRoles, state)) return fallback;
  return 'var(--machine-' + index + '-' + state + ', ' + fallback + ')';
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
  paintPalette();
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
    byId('command-result').hidden = true;
    hideSuggestions();
    stopTerminal();
  }
  if (!active || (opened && selection.has(route) && selection.get(route) !== activeKey)) opened = false;
  byId('workspace').dataset.mobileView = opened ? 'detail' : 'list';
  paintHeader();
  paintCollection();
  paintDetail();
}

function stateDot(state, machine) {
  const dot = make('span', 'state-dot');
  dot.style.setProperty('--state-color', stateToken(state, machine));
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
    chip.append(stateDot(state, nodes[0]), make('b', '', String(count)));
    if (state === activeFilter) requestAnimationFrame(() => chip.focus({ preventScroll: true }));
    return chip;
  }));
}

function onHold(row, activate, mouseMenu, setPressed) {
  let timer;
  let start;
  let held = false;
  const stop = () => {
    clearTimeout(timer);
    if (start) setPressed?.(false);
    start = null;
  };
  const run = () => {
    if (held || !row.isConnected || overlay.open) return;
    held = true;
    suppressHoldClick = true;
    activate();
  };
  row.addEventListener('pointerdown', (event) => {
    stop();
    held = false;
    if (event.pointerType === 'mouse' || !event.isPrimary || event.target.closest('input')) return;
    start = [event.clientX, event.clientY];
    setPressed?.(true);
    timer = setTimeout(run, 480);
  });
  row.addEventListener('pointermove', (event) => {
    if (start && Math.hypot(event.clientX - start[0], event.clientY - start[1]) > 9) stop();
  });
  for (const event of ['pointerup', 'pointercancel', 'pointerleave']) row.addEventListener(event, stop);
  row.addEventListener('contextmenu', (event) => {
    if (!start && !held) {
      if (event.pointerType !== 'touch' && !overlay.open) mouseMenu?.(event);
      return;
    }
    event.preventDefault();
    const show = start;
    stop();
    if (show) run();
  });
}

function itemMenu(container, open) {
  const show = () => {
    open();
    trigger.setAttribute('aria-expanded', 'true');
    overlay.addEventListener('close', () => trigger.setAttribute('aria-expanded', 'false'), { once: true });
  };
  container.classList.add('has-item-menu');
  onHold(container, show, (event) => {
    event.preventDefault();
    show();
  }, (pressed) => container.classList.toggle('is-pressed', pressed));
  const trigger = button('', 'row-action item-menu', show);
  trigger.title = t('itemActions');
  trigger.setAttribute('aria-label', trigger.title);
  trigger.setAttribute('aria-haspopup', 'dialog');
  trigger.setAttribute('aria-expanded', 'false');
  trigger.append(glyph('more'));
  container.append(trigger);
}

function confirmClose(title, machine, payload) {
  const agent = payload.scope === 'agent';
  const label = t(agent ? 'closeAgent' : 'closePane');
  const form = make('div', 'confirm-actions');
  const cancel = button(t('cancel'), 'secondary', closeSheet);
  const submit = button(label, 'secondary danger', async () => {
    submit.disabled = true;
    try {
      await api('/api/close', 'POST', payload);
      if (submit.isConnected) closeSheet();
      if (!agent) {
        for (const [record, pane] of selectedPane) {
          if (record.startsWith(payload.node + '/') && pane === payload.pane) selectedPane.delete(record);
        }
      }
      await refresh(true);
    } catch (error) {
      if (submit.isConnected) closeSheet();
      await refresh();
      toast(error.message);
    }
  });
  form.append(cancel, submit);
  sheet(label, make('p', 'sheet-target', title + ' · ' + machine),
    make('p', 'sheet-copy', t(agent ? 'confirmAgentClose' : 'confirmPaneClose')), form);
  cancel.focus();
}

function closeChoice(title, machine, payload, allowed, exited = false) {
  const row = choice(t(payload.scope === 'agent' ? 'closeAgent' : 'closePane'),
    allowed ? '' : t(exited ? 'agentExited' : 'closeIdleOnly'),
    () => confirmClose(title, machine, payload));
  row.prepend(glyph('close'));
  row.classList.add('danger');
  row.disabled = !allowed;
  return row;
}

function showRecordMenu(item, machine, pinned) {
  const session = item.sessionRecord;
  const title = item.worker?.name || item.title;
  const actions = [];
  if (machine.sidebar && item.pane && !item.worker) {
    const target = session ? item.id : item.pane.window;
    const assistant = !session && machine.pinnedAgents?.some((card) => card.pane?.window === target && card.assistant);
    const pin = choice(t(pinned ? 'unpin' : 'pin'), assistant ? t('assistantPinned') : '', async () => {
      closeSheet();
      try {
        await api('/api/pins', 'POST', {
          node: machine.id, scope: session ? 'session' : 'agent', target, pane: item.pane.key, pinned: !pinned,
        });
        await refresh(true);
      } catch (error) { toast(error.message); }
    });
    pin.prepend(glyph('pin'));
    pin.disabled = Boolean(assistant);
    actions.push(pin);
  }
  if (!session && item.pane && item.kind !== 'shell') {
    const payload = { node: machine.id, scope: 'agent', agent: item.id, pane: item.pane.key };
    actions.push(closeChoice(title, machine.name, payload, item.canClose, item.exited));
  }
  if (actions.length) sheet(title, ...actions);
}

function recordRow(item, machine, pinned, focusKey, canExpand = true) {
  const session = item.sessionRecord;
  const container = make('div', 'record-row' + (item.worker ? ' worker-row' : ''));
  const row = button('', 'agent-row', () => openRecord(item));
  row.dataset.record = recordKey(item);
  if (row.dataset.record === activeKey && (opened || !mobile.matches)) row.setAttribute('aria-current', 'true');
  const icon = make('span', 'agent-tag');
  const provider = Object.hasOwn(providerMarks, item.kind) && providerMarks[item.kind];
  if (provider) {
    const mark = make('img', 'provider-mark');
    mark.alt = '';
    mark.draggable = false;
    mark.src = '/providers/' + provider + '?v=' + appVersion;
    icon.classList.add('brand');
    icon.append(mark);
  } else icon.append(glyph(session ? 'sessions' : 'terminal'));
  icon.setAttribute('aria-hidden', 'true');
  const copy = make('span', 'record-copy');
  const title = item.worker ? item.worker.name : item.title;
  const subtype = session ? [t('agentCount', { count: item.agentCount }), item.branch].filter(Boolean).join(' · ')
    : [item.worker?.title || kindNames[item.kind], machine.sidebar?.sort === 'grouped' ? '' : item.session].filter(Boolean).join(' · ');
  copy.append(make('b', '', title), make('span', 'agent-meta', subtype));
  if (item.exited) copy.append(make('span', 'agent-meta', t('agentExited')));
  const badge = make('span', 'record-state');
  badge.title = stateNames[item.state] || '';
  badge.append(stateDot(item.state, machine), make('span', 'sr-only', badge.title));
  row.append(icon, copy, badge);
  container.append(row);
  if (row.dataset.record === focusKey) requestAnimationFrame(() => row.focus({ preventScroll: true }));
  if (session && canExpand && item.windows?.length) {
    const key = recordKey(item);
    const expanded = expandedSessions.has(key);
    const toggle = button('', 'row-action session-toggle', () => {
      if (expanded) expandedSessions.delete(key);
      else expandedSessions.add(key);
      paintWorkspace();
      for (const trigger of document.querySelectorAll('.session-toggle')) {
        if (trigger.dataset.session === key) { trigger.focus({ preventScroll: true }); break; }
      }
    });
    toggle.append(glyph('next'));
    toggle.dataset.session = key;
    toggle.title = t(expanded ? 'collapseSession' : 'expandSession', { session: item.title });
    toggle.setAttribute('aria-label', toggle.title);
    toggle.setAttribute('aria-expanded', String(expanded));
    container.prepend(toggle);
  }
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
  const canPin = machine.sidebar && item.pane && !item.worker;
  const hasAgent = !session && item.pane && item.kind !== 'shell';
  if (pinned) {
    const marker = glyph('pin');
    marker.classList.add('pin-marker');
    badge.prepend(marker, make('span', 'sr-only', t('pinned')));
  }
  if (canPin || hasAgent) {
    itemMenu(container, () => showRecordMenu(item, machine, pinned));
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
      const sort = make('button', 'picker-button sort-picker');
      const label = t('sortOrder', { machine: machine.name });
      const options = [
        { value: 'grouped', label: t(route === 'agents' ? 'groupBySession' : 'sortByName') },
        { value: 'prioritized', label: t('prioritized') },
      ];
      const updateSort = async (value) => {
        sort.disabled = true;
        try {
          await api('/api/sidebar-sort', 'POST', { node: machine.id, sort: value });
          await refresh(true);
        } catch (error) {
          setPicker(sort, label, options, machine.sidebar.sort, updateSort);
          toast(error.message);
        }
        finally { sort.disabled = false; }
      };
      setPicker(sort, label, options, machine.sidebar.sort, updateSort);
      heading.append(sort);
    }
    group.append(heading);
    const pinnedIds = new Set(pinned.map((item) => route === 'sessions' ? item.id : item.pane?.window));
    if (pinned.length) {
      const rack = make('div', 'pinned-records');
      rack.append(make('h4', 'subheading', t('pinned')), ...pinned.map((item) => recordRow(item, machine, true, focusKey, false)));
      group.append(rack);
    }
    if (!rows.length && !pinned.length) {
      const message = machine.state === 'pending' ? 'waitingForPairing' : machine.state === 'offline' ? 'nodeReconnect'
        : !machine.snapshotAvailable ? 'snapshotUnavailable' : route === 'sessions' ? 'noSessions'
          : machine.agents?.length ? 'noMatchingAgents' : 'noAgents';
      group.append(make('div', 'empty', t(message)));
    }
    for (const item of rows) {
      if (item.group !== undefined) { group.append(make('h4', 'subheading', item.group)); continue; }
      group.append(recordRow(item, machine, pinnedIds.has(route === 'sessions' ? item.id : item.pane?.window), focusKey));
      if (route === 'sessions' && expandedSessions.has(recordKey(item))) group.append(renderSessionTree(item));
    }
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
  byId('chat').hidden = !(route === 'agents' && supportsChat());
  if (route !== 'sessions' || !active) byId('session-tree').hidden = true;
  paintMessageSelection();
  if (route !== 'sessions' || !active) byId('term').hidden = true;
  if (!active) {
    empty.hidden = false;
    empty.textContent = t(route === 'sessions' ? 'selectSession' : 'chooseMachine');
    return;
  }
  byId('detail-name').textContent = active.title;
  byId('detail-context').textContent = [active.host, active.session || active.branch || kindNames[active.kind]].filter(Boolean).join(' / ');
  if (route === 'sessions') { paintSessionPanes(empty); return; }
  stopTerminal();
  empty.hidden = supportsChat();
  if (!supportsChat()) { empty.textContent = t('noReadableHistory'); return; }
  if (!opened && mobile.matches) return;
  paintComposer();
  const key = conversationKey();
  if (transcript?.key !== key) {
    transcript = { key, rows: new Map(), before: null, latest: -1, loaded: !active.thread, selected: null };
    byId('messages').replaceChildren(make('p', active.thread ? 'day' : 'empty', t(active.thread ? 'readingHistory' : 'noMessages')));
    byId('history-progress').hidden = true;
    paintMessageNavigation();
    paintOutgoing();
    loadHistory();
  }
}

function renderSessionTree(session) {
  const tree = make('div', 'pane-tree');
  for (const window of session.windows || []) {
    const group = make('section', 'window-group');
    group.append(make('h4', 'subheading', window.index + ': ' + window.title));
    for (const pane of window.panes) {
      const title = t('sessionPane', { index: pane.index, pane: pane.pane });
      const container = make('div', 'pane-actions');
      const open = () => {
        closeSheet();
        selectedPane.set(recordKey(session), pane.key);
        if (opened && activeKey === recordKey(session)) paintDetail();
        else openRecord(session);
      };
      const row = button(title, 'pane-row', open);
      row.prepend(glyph('terminal'));
      container.append(row);
      itemMenu(container, () => {
        const terminal = choice(t('openTerminal'), '', open);
        terminal.prepend(glyph('terminal'));
        const payload = { node: session.node, scope: 'pane', pane: pane.key };
        sheet(title, terminal, closeChoice(title, session.host, payload, pane.canClose));
      });
      group.append(container);
    }
    tree.append(group);
  }
  return tree;
}

function paintSessionPanes(empty) {
  const windows = opened ? active.windows || [] : [];
  const selected = selectedPane.get(activeKey);
  const pane = windows.flatMap((window) => window.panes).find((item) => item.key === selected);
  const tree = byId('session-tree');
  tree.hidden = Boolean(pane) || !windows.length;
  if (!tree.hidden) tree.replaceChildren(renderSessionTree(active));
  empty.hidden = Boolean(pane) || !tree.hidden;
  byId('term').hidden = !pane;
  if (!pane) {
    stopTerminal();
    if (!empty.hidden) empty.textContent = t(!opened ? 'selectSession' : 'noSessionPane');
    return;
  }
  const picker = byId('terminal-pane');
  const session = activeKey;
  const options = windows.flatMap((window) => window.panes.map((item) => ({
    value: item.key, label: t('sessionPane', { index: item.index, pane: item.pane }),
    group: window.index + ': ' + window.title,
  })));
  setPicker(picker, t('sessionPanes'), options, pane.key, (key) => {
    if (route !== 'sessions' || activeKey !== session) return;
    selectedPane.set(session, key);
    paintDetail();
  });
  startTerminal(pane);
}

function paintComposer() {
  const key = conversationKey();
  if (chatInput.dataset.thread !== key) {
    chatInput.dataset.thread = key;
    chatInput.value = drafts.get(key)?.text || '';
  }
  const ready = (active.thread ? ['idle', 'done', 'working'].includes(active.state) : active.state === 'idle') && !active.pane.inMode;
  const working = active.state === 'working';
  const sendButton = byId('send');
  sendButton.disabled = !ready || submitting || reauthRequired;
  for (const retry of byId('outgoing-messages').querySelectorAll('.delivery-failed')) retry.disabled = sendButton.disabled;
  const label = t(working ? 'agentWorking' : active.state === 'blocked' ? 'agentBlocked'
    : ready ? 'originalConversation' : 'agentInputUnavailable');
  const display = byId('chat-status');
  if (display.textContent !== label) display.textContent = label;
  byId('working-dots').hidden = !working;
  const stop = byId('stop-agent');
  const pendingStop = stopping?.key === activeKey;
  if (pendingStop && !working) stopping = null;
  stop.hidden = !working || !active.chat || active.pane.inMode;
  stop.disabled = pendingStop || submitting || reauthRequired;
  const stopLabel = t(pendingStop ? 'stoppingTurn' : 'stopCurrentTurn');
  stop.setAttribute('aria-label', stopLabel);
  stop.title = stopLabel;
  sendButton.hidden = !stop.hidden && !chatInput.value.trim();
  stop.classList.toggle('secondary-stop', !sendButton.hidden);
  const settings = byId('chat-settings');
  settings.hidden = !active.chat;
  settings.disabled = reauthRequired;
  settings.title = t('modelAndThinking');
  const queued = nextSettings.get(key);
  const selected = queued?.model ? queued : chatModels.get(key);
  byId('chat-settings-label').textContent = selected?.model
    ? [selected.label, selected.effort && t('effort_' + selected.effort), queued?.model && t('nextTurn')].filter(Boolean).join(' · ')
    : t('modelAndThinking');
  const permissions = byId('chat-permission');
  permissions.hidden = !active.chat;
  permissions.disabled = reauthRequired;
  permissions.classList.toggle('next-setting', Boolean(queued?.permission));
  permissions.title = queued?.permission ? t('permission') + ' · ' + t('nextTurn') : t('permission');
  byId('composer-label').style.setProperty('--state-color', stateToken(active.state, { id: active.node }));
  if (overlay.open && modelEditor?.form.isConnected) modelEditor.update();
  resizeComposer();
}

function resizeComposer() {
  if (autoSizeComposer || !chatInput.clientWidth) return;
  chatInput.style.height = 'auto';
  chatInput.style.height = chatInput.scrollHeight + 'px';
}

async function stopAgent() {
  const stop = byId('stop-agent');
  if (stop.hidden || stop.disabled || !active) return;
  const item = active;
  const request = { key: activeKey };
  const repaint = () => { if (route === 'agents' && supportsChat()) paintComposer(); };
  stopping = request;
  repaint();
  try {
    await api('/api/interrupt', 'POST', { node: item.node, pane: item.pane.key, agent: item.id, thread: item.thread });
    await refresh(true);
    if (activeKey === request.key) loadHistory();
  } catch (error) {
    if (stopping === request) stopping = null;
    if (signedIn) toast(error.message);
    repaint();
  }
  setTimeout(() => {
    if (stopping !== request) return;
    stopping = null;
    repaint();
  }, 3500);
}

async function showChatSettings() {
  const control = byId('chat-settings');
  if (control.hidden || control.disabled || !active) return;
  const key = conversationKey();
  const queued = nextSettings.get(key);
  const { node, id: agent, thread, pane: { key: pane } } = active;
  const target = { node, agent, pane, thread };
  const wait = make('p', 'sheet-note', t('loadingModelOptions'));
  sheet(t('modelAndThinking'), wait);
  try {
    const { models, model, effort } = await api(pathWith('/api/chat-settings', { node, agent, pane, ...(thread ? { thread } : {}) }));
    if (!overlay.open || !wait.isConnected || key !== conversationKey()) return;
    const chosen = models.find(({ id }) => id === model);
    if (chosen) chatModels.set(key, { model, effort, label: chosen.label });
    paintComposer();
    const form = make('form', 'workspace-form');
    const modelPicker = make('button', 'picker-button');
    const effortPicker = make('button', 'picker-button');
    const modelLabel = make('label', '', t('model'));
    const effortLabel = make('label', '', t('thinkingLevel'));
    modelLabel.append(modelPicker);
    effortLabel.append(effortPicker);
    const submit = make('button', 'primary', t('applyChatSettings'));
    submit.type = 'submit';
    const availability = make('p', 'sheet-note', t('settingsNextTurn'));
    let applying = false;
    const update = () => {
      availability.hidden = settingsReady();
      submit.disabled = applying || reauthRequired || conversationKey() !== key;
    };
    const error = make('p', 'message-error');
    error.setAttribute('role', 'alert');
    error.hidden = true;

    const updateEfforts = (value, preferred) => {
      const selected = models.find(({ id }) => id === value);
      const options = selected.efforts.map((level) => ({ value: level, label: t('effort_' + level) }));
      setPicker(effortPicker, t('thinkingLevel'), options, preferred || selected.defaultEffort);
      effortLabel.hidden = !options.length;
    };
    setPicker(modelPicker, t('model'), models.map(({ id, label }) => ({ value: id, label })), queued?.model || model, updateEfforts);
    updateEfforts(modelPicker.value, queued?.effort ?? effort);
    form.append(modelLabel, effortLabel, availability, error, submit);
    form.addEventListener('submit', async (event) => {
      event.preventDefault();
      if (submit.disabled) return;
      const selected = models.find(({ id }) => id === modelPicker.value);
      const thinking = effortPicker.value || null;
      if (!settingsReady()) {
        rememberSettings(key, { ...nextSettings.get(key), model: selected.id, effort: thinking, label: selected.label });
        closeSheet();
        paintComposer();
        return;
      }
      applying = true;
      submit.disabled = modelPicker.disabled = effortPicker.disabled = true;
      error.hidden = true;
      try {
        const saved = await api('/api/chat-settings', 'POST', { ...target, model: selected.id, effort: thinking });
        if (signedIn && active?.pane?.key === pane && active.node === node && active.id === agent) {
          const current = conversationKey();
          chatModels.set(current, { ...saved, label: selected.label });
          rememberSettings(current, { permission: nextSettings.get(current)?.permission });
          paintComposer();
        }
        if (form.isConnected) closeSheet();
      } catch (failure) {
        error.textContent = failure.message;
        error.hidden = false;
      } finally {
        applying = false;
        modelPicker.disabled = false;
        effortPicker.disabled = !selected.efforts.length;
        update();
      }
    });
    byId('sheet-body').replaceChildren(form);
    modelEditor = { form, update };
    update();
    modelPicker.focus();
  } catch (error) {
    if (wait.isConnected) wait.textContent = error.message;
  }
}

function settingsReady() {
  return ['idle', 'done'].includes(active?.state) && !active.pane.inMode && !submitting;
}

function chatTarget(item = active) {
  return { node: item.node, agent: item.id, pane: item.pane.key, thread: item.thread || null };
}

function chatQuery(target, kind) {
  const { thread, ...query } = target;
  return { ...query, ...(thread ? { thread } : {}), ...(kind ? { kind } : {}) };
}

function suggestionToken() {
  if (composingPrompt || reauthRequired || route !== 'agents' || !active?.chat
      || chatInput.selectionStart !== chatInput.selectionEnd) return null;
  const end = chatInput.selectionStart;
  const before = chatInput.value.slice(0, end);
  const command = /^\/([a-z0-9-]*)$/i.exec(before);
  const skill = /(?:^|\s)\$([a-z][a-z0-9._:/-]*)?$/i.exec(before);
  if (!command && !skill) return null;
  const prefix = (command || skill)[1] || '';
  return { kind: command ? 'commands' : 'skills', prefix, start: end - prefix.length - 1, end };
}

function hideSuggestions() {
  byId('chat-suggestions').hidden = true;
  chatInput.removeAttribute('aria-activedescendant');
  suggestionItems = [];
  suggestionIndex = 0;
}

function chooseSuggestion(item) {
  const token = suggestionToken();
  if (!token) return;
  const prefix = token.kind === 'commands' ? '/' : '$';
  chatInput.setRangeText(prefix + item.name + ' ', token.start, token.end, 'end');
  rememberDraft(conversationKey(), chatInput.value);
  hideSuggestions();
  paintComposer();
  chatInput.focus({ preventScroll: true });
}

function updateSuggestions() {
  const token = suggestionToken();
  if (!token) { hideSuggestions(); return; }
  const list = byId('chat-suggestions');
  const key = conversationKey() + '/' + token.kind;
  let data = suggestionCache.get(key);
  if (!data || data.until <= Date.now()) {
    const target = chatTarget();
    data = { until: Infinity };
    suggestionCache.set(key, data);
    if (suggestionCache.size > 64) suggestionCache.delete(suggestionCache.keys().next().value);
    const request = token.kind === 'commands' ? api('/api/chat-commands', 'POST', target)
      : api(pathWith('/api/chat-capabilities', chatQuery(target, token.kind)));
    data.job = request.then(({ suggestions }) => { data.options = suggestions; }, (error) => { data.error = error.message; })
      .finally(() => {
        data.job = null;
        data.until = Date.now() + (data.error ? 2000 : 30_000);
        if (document.activeElement === chatInput && conversationKey() + '/' + suggestionToken()?.kind === key) updateSuggestions();
      });
  }
  const query = token.prefix.toLowerCase();
  suggestionItems = (data.options || []).filter(({ name, label }) =>
    !query || name.toLowerCase().includes(query) || label.toLowerCase().includes(query));
  suggestionIndex = Math.min(suggestionIndex, Math.max(0, suggestionItems.length - 1));
  list.hidden = false;
  if (!suggestionItems.length) {
    chatInput.removeAttribute('aria-activedescendant');
    list.replaceChildren(make('p', 'sheet-note', data.error || t(data.job ? 'loadingSuggestions' : 'noSuggestions')));
    return;
  }
  list.replaceChildren(...suggestionItems.map((item, index) => {
    const row = choice(token.kind === 'skills' ? '$' + item.name : item.label, item.description, () => chooseSuggestion(item));
    row.id = 'suggestion-' + index;
    row.setAttribute('role', 'option');
    row.setAttribute('aria-selected', String(index === suggestionIndex));
    row.tabIndex = -1;
    row.addEventListener('pointerdown', (event) => event.preventDefault());
    return row;
  }));
  chatInput.setAttribute('aria-activedescendant', 'suggestion-' + suggestionIndex);
}

async function runSlash(text) {
  const key = conversationKey();
  const item = active;
  const command = text.trim();
  const clear = (value = '') => {
    if (conversationKey() !== key || chatInput.value !== text) return;
    chatInput.value = value;
    rememberDraft(key, value);
    paintComposer();
  };
  hideSuggestions();
  if (command === '/skills') {
    clear('$');
    chatInput.focus({ preventScroll: true });
    updateSuggestions();
    return;
  }
  if (command === '/model' || command === '/permissions') {
    clear();
    if (command === '/model') showChatSettings();
    else showPermissionSettings();
    return;
  }
  submitting = true;
  paintComposer();
  try {
    await suggestionCache.get(key + '/commands')?.job;
    if (!signedIn || conversationKey() !== key) return;
    const { output } = await api('/api/chat-command', 'POST', { ...chatTarget(item), text: command });
    if (conversationKey() !== key) return;
    clear();
    byId('command-result-text').textContent = output || t('commandSent');
    byId('command-result').hidden = false;
    refresh(true);
  } catch (error) { if (signedIn && conversationKey() === key) toast(error.message); }
  finally {
    submitting = false;
    if (route === 'agents' && supportsChat()) paintComposer();
  }
}

const permissionLabels = {
  ':workspace': 'workspacePermission', ':read-only': 'readOnlyPermission', ':danger-full-access': 'fullAccessPermission',
};

async function showPermissionSettings() {
  if (!active?.chat || byId('chat-permission').disabled) return;
  const key = conversationKey();
  const target = chatTarget();
  const loading = make('p', 'sheet-note', t('loadingPermissions'));
  sheet(t('permission'), loading);
  try {
    const { profiles } = await api(pathWith('/api/chat-capabilities', chatQuery(target, 'permissions')));
    if (!overlay.open || !loading.isConnected || conversationKey() !== key) return;
    const options = profiles.map(({ id }) => ({ value: id, label: permissionLabels[id] ? t(permissionLabels[id]) : id }));
    if (!options.length) { loading.textContent = t('noPermissions'); return; }
    const form = make('form', 'workspace-form');
    const picker = make('button', 'picker-button');
    const description = make('p', 'sheet-note');
    const submit = make('button', 'primary', t('applyChatSettings'));
    const error = make('p', 'message-error');
    error.hidden = true;
    submit.type = 'submit';
    const update = () => {
      description.textContent = profiles.find(({ id }) => id === picker.value)?.description
        || (!settingsReady() ? t('settingsNextTurn') : '');
      description.hidden = !description.textContent;
      submit.disabled = !picker.value || reauthRequired || conversationKey() !== key;
    };
    setPicker(picker, t('permission'), [{ value: '', label: t('choosePermission') }, ...options],
      nextSettings.get(key)?.permission || chatPermissions.get(key), update);
    form.append(picker, description, error, submit);
    form.addEventListener('submit', async (event) => {
      event.preventDefault();
      if (submit.disabled) return;
      const permission = picker.value;
      if (!settingsReady()) {
        rememberSettings(key, { ...nextSettings.get(key), permission });
        closeSheet();
        paintComposer();
        return;
      }
      picker.disabled = submit.disabled = true;
      error.hidden = true;
      try {
        await api('/api/chat-permission', 'POST', { ...target, permission });
        chatPermissions.set(key, permission);
        rememberSettings(key, { ...nextSettings.get(key), permission: null });
        if (form.isConnected) closeSheet();
        if (conversationKey() === key) paintComposer();
      } catch (failure) {
        error.textContent = failure.message;
        error.hidden = false;
      } finally {
        picker.disabled = false;
        update();
      }
    });
    byId('sheet-body').replaceChildren(form);
    update();
    picker.focus();
  } catch (error) { if (loading.isConnected) loading.textContent = error.message; }
}

function loadHistory(older = false) {
  const target = transcript;
  if (reauthRequired || !active?.thread || !target || (older && !target.before)) return;
  if (target.loading) return target.loading;
  const query = { node: active.node, agent: active.id, thread: active.thread };
  if (older) query.before = target.before;
  target.loading = readHistory(target, query, older);
  return target.loading;
}

async function readHistory(target, query, older) {
  if (older) byId('history-progress').hidden = false;
  let pageLoaded = false;
  try {
    const { messages, before } = await api(pathWith('/api/history', query));
    if (target !== transcript) return;
    const previous = target.loaded;
    if (!previous) target.before = before;
    if (older) target.before = Number.isSafeInteger(before) && before > 0 && before < target.before ? before : null;
    target.loaded = true;
    const added = messages.filter(({ id }) => !target.rows.has(id));
    for (const message of added) {
      target.rows.set(message.id, message);
      if (Number.isSafeInteger(message.id)) target.latest = Math.max(target.latest, message.id);
    }
    if (added.length || !previous) renderHistory(target, added, older);
    pageLoaded = true;
  } catch (error) {
    if (target !== transcript || !signedIn) return;
    if (!target.loaded) byId('messages').replaceChildren(make('div', 'empty', error.status === 404 ? t('noReadableHistory') : error.message));
  } finally {
    target.loading = null;
    if (target === transcript) {
      byId('history-progress').hidden = true;
      paintMessageNavigation();
      if (pageLoaded) requestAnimationFrame(loadEarlierAtTop);
    }
  }
  return pageLoaded;
}

function loadEarlierAtTop() {
  const scroller = byId('history');
  if (route === 'agents' && scroller.clientHeight && scroller.scrollTop < 60) loadHistory(true);
}

function userMessageTargets(target, scroller) {
  const rows = [...scroller.querySelectorAll('.message.human')];
  const anchor = target.messageAnchor;
  if (anchor && Math.abs(scroller.scrollTop - anchor.scrollTop) < 1) {
    const index = rows.indexOf(anchor.row);
    if (index >= 0) return { previous: rows[index - 1], next: rows[index + 1] };
  }
  target.messageAnchor = null;
  const top = scroller.getBoundingClientRect().top + 8;
  return {
    previous: rows.findLast((row) => row.getBoundingClientRect().top < top - 1),
    next: rows.find((row) => row.getBoundingClientRect().top > top + 1),
  };
}

function paintMessageNavigation() {
  const target = transcript;
  const scroller = byId('history');
  const navigation = byId('message-navigation');
  navigation.hidden = route !== 'agents' || !supportsChat() || !target?.loaded
    || target.key !== conversationKey() || Boolean(target.selected) || !scroller.clientHeight
    || (!target.before && scroller.scrollHeight <= scroller.clientHeight);
  if (navigation.hidden) return;
  const { previous, next } = userMessageTargets(target, scroller);
  byId('previous-message').disabled = target.jumping || !(previous || target.before);
  byId('next-message').disabled = target.jumping || !next;
}

async function jumpToUserMessage(direction) {
  if (byId(direction + '-message').disabled || byId('message-navigation').hidden) return;
  const target = transcript;
  const scroller = byId('history');
  target.jumping = true;
  paintMessageNavigation();
  try {
    let row = userMessageTargets(target, scroller)[direction];
    while (!row && direction === 'previous' && target.before) {
      if (!await loadHistory(true) || target !== transcript || target.selected
          || route !== 'agents' || !scroller.clientHeight) return;
      row = userMessageTargets(target, scroller).previous;
    }
    if (row) {
      scroller.scrollTop += row.getBoundingClientRect().top - scroller.getBoundingClientRect().top - 8;
      target.messageAnchor = { row, scrollTop: scroller.scrollTop };
      if (!matchMedia('(prefers-reduced-motion: reduce)').matches) {
        row.animate([{ backgroundColor: 'var(--active)' }, { backgroundColor: 'transparent' }], 700);
      }
    }
  } finally {
    target.jumping = false;
    if (target === transcript) paintMessageNavigation();
  }
}

async function copyMessages(rows) {
  if (!rows.length) return;
  try {
    await navigator.clipboard.writeText(rows.map(({ text }) => text).join('\n\n'));
    toast(t('messagesCopied', { count: rows.length }));
  } catch { toast(t('copyFailed')); }
}

function selectedMessages() {
  if (!transcript?.selected) return [];
  return [...transcript.selected].sort((a, b) => a - b).map((id) => transcript.rows.get(id));
}

async function shareMessages() {
  const rows = selectedMessages();
  if (!rows.length || typeof navigator.share !== 'function') return;
  try {
    await navigator.share({ text: rows.map(({ text }) => text).join('\n\n') });
  } catch (error) {
    if (error.name !== 'AbortError') toast(t('shareFailed'));
  }
}

function clearMessageSelection() {
  if (!transcript?.selected) return false;
  transcript.selected = null;
  for (const pick of byId('messages').querySelectorAll('.message-pick:checked')) {
    pick.checked = false;
    pick.closest('.message').classList.remove('picked');
  }
  paintMessageSelection();
  return true;
}

function exitMessageSelection() {
  if (route !== 'agents' || !transcript) return false;
  const state = history.state;
  const selectionEntry = state?.messageSelection === transcript.key;
  const cleared = clearMessageSelection();
  if (cleared && selectionEntry) history.go(state.linkPreviewDismissed ? -2 : -1);
  return cleared || selectionEntry;
}

function messageMenu(event, select) {
  if (transcript.selected || window.getSelection()?.toString()) return;
  event.preventDefault();
  const action = choice(t('selectMessages'), '', () => {
    closeSheet();
    select();
  });
  action.prepend(glyph('select'));
  sheet(t('messageActions'), action);
  overlay.dataset.position = 'context';
  const bounds = overlay.querySelector('.sheet').getBoundingClientRect();
  overlay.style.setProperty('--menu-x', Math.max(8, Math.min(event.clientX, innerWidth - bounds.width - 8)) + 'px');
  overlay.style.setProperty('--menu-y', Math.max(8, Math.min(event.clientY, innerHeight - bounds.height - 8)) + 'px');
  action.focus();
}

function openLinkPreview(link) {
  const url = new URL(link.href);
  if (!['http:', 'https:'].includes(url.protocol) || url.origin === location.origin) return false;
  if (!linkTabs.has(url.href)) {
    const label = link.textContent.trim() || url.hostname;
    linkTabs.set(url.href, { id: 'link-preview-' + ++nextLinkId, label, frame: null });
  }
  if (!activeLink && !history.state?.linkPreview) pushChatLayer({ linkPreview: true });
  linkOpener = link;
  activeLink = url.href;
  paintLinkPanel();
  if (fullScreenPreview.matches) focus('close-link-panel');
  return true;
}

function closeLinkPanel() {
  activeLink = '';
  paintLinkPanel();
  if (linkOpener?.isConnected) linkOpener.focus({ preventScroll: true });
  const { linkPreview, messageSelection } = history.state || {};
  if (linkPreview && messageSelection) {
    history.replaceState({ ...history.state, linkPreview: false, linkPreviewDismissed: true }, '');
  } else if (linkPreview) history.back();
}

function selectLinkTab(url) {
  activeLink = url;
  paintLinkPanel();
  document.getElementById(linkTabs.get(url).id + '-tab').focus();
}

function closeLinkTab(url) {
  const keys = [...linkTabs.keys()];
  const tab = linkTabs.get(url);
  tab.frame?.remove();
  linkTabs.delete(url);
  if (!linkTabs.size) closeLinkPanel();
  else selectLinkTab(url === activeLink ? keys[keys.indexOf(url) + 1] || keys.at(-2) : activeLink);
}

function paintLinkPanel() {
  const panel = byId('link-panel');
  const visible = route === 'agents' && linkTabs.has(activeLink);
  panel.hidden = !visible;
  app.toggleAttribute('data-link-preview', visible);
  if (!visible) {
    for (const tab of linkTabs.values()) {
      tab.frame?.remove();
      tab.frame = null;
    }
    return;
  }
  byId('open-link-browser').href = activeLink;
  const pages = byId('link-pages');
  const tabs = byId('link-tabs');
  tabs.replaceChildren(...[...linkTabs].map(([url, tab]) => {
    const selected = url === activeLink;
    if (selected && !tab.frame) {
      const frame = make('iframe', 'link-frame');
      frame.id = tab.id;
      frame.title = tab.label;
      frame.setAttribute('sandbox', 'allow-scripts allow-forms');
      frame.referrerPolicy = 'no-referrer';
      frame.src = url;
      pages.append(frame);
      tab.frame = frame;
    }
    if (tab.frame) tab.frame.hidden = !selected;
    const controls = make('div', 'link-tab');
    controls.setAttribute('role', 'presentation');
    const title = button(tab.label, 'link-tab-title', () => selectLinkTab(url));
    title.id = tab.id + '-tab';
    title.title = url;
    title.setAttribute('role', 'tab');
    title.setAttribute('aria-selected', String(selected));
    title.tabIndex = selected ? 0 : -1;
    if (tab.frame) title.setAttribute('aria-controls', tab.id);
    const close = button('', 'link-tab-close', () => closeLinkTab(url));
    close.title = t('closeLinkTab', { page: tab.label });
    close.setAttribute('aria-label', close.title);
    close.append(glyph('close'));
    controls.append(title, close);
    return controls;
  }));
  const current = tabs.querySelector('[aria-selected="true"]');
  if (current.offsetLeft < tabs.scrollLeft || current.offsetLeft + current.offsetWidth > tabs.scrollLeft + tabs.clientWidth) {
    current.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  }
}

function paintMessageSelection() {
  const count = transcript?.selected?.size || 0;
  const selecting = Boolean(transcript?.selected);
  byId('chat-menu').hidden = selecting || route !== 'agents' || !supportsChat() || !active.pane;
  byId('chat-actions').hidden = !selecting || route !== 'agents' || !supportsChat()
    || transcript?.key !== conversationKey() || !transcript.rows.size;
  byId('back').setAttribute('aria-label', t(selecting ? 'cancelMessageSelection'
    : route === 'agents' ? 'backToAgents' : 'backToSessions'));
  byId('messages').classList.toggle('selecting', selecting);
  byId('copy-selection').hidden = !selecting;
  byId('copy-selection').disabled = !count;
  byId('share-selection').hidden = !selecting || typeof navigator.share !== 'function';
  byId('share-selection').disabled = !count;
  const copyLabel = t('copySelectedMessages', { count });
  byId('copy-selection').setAttribute('aria-label', copyLabel);
  byId('copy-selection').title = copyLabel;
  byId('selection-count').textContent = String(count);
  paintMessageNavigation();
}

function messageContent(text, human) {
  const body = make('div', human ? 'message-content bubble' : 'message-content');
  body.innerHTML = markdown.render(text);
  for (const link of body.querySelectorAll('a')) {
    link.target = '_blank';
    link.rel = 'noopener noreferrer';
  }
  return body;
}

function matchingText(text) {
  return text.replace(/\r\n?/g, '\n').trim();
}

function matchOutgoing(queue, messages) {
  if (!queue?.entries.length) return false;
  let changed = false;
  for (const message of messages) {
    if (message.role !== 'user' || queue.matched.has(message.id)) continue;
    const text = matchingText(message.text);
    const index = queue.entries.findIndex((entry) => text === entry.match && (entry.after !== null
      ? message.id >= entry.after : entry.state !== 'sending' && Date.parse(message.time) >= entry.startedAt));
    if (index >= 0) {
      const [entry] = queue.entries.splice(index, 1);
      cacheChat(deliveryStorage + entry.id, null);
      if (drafts.get(entry.key)?.retry === entry.id) {
        rememberDraft(entry.key, '');
        if (chatInput.dataset.thread === entry.key) {
          chatInput.value = '';
          resizeComposer();
        }
      }
      queue.matched.add(message.id);
      changed = true;
    }
  }
  return changed;
}

function paintOutgoing() {
  const container = byId('outgoing-messages');
  const entries = outgoing.get(conversationKey())?.entries || [];
  container.hidden = !entries.length;
  byId('messages').toggleAttribute('data-outgoing', Boolean(entries.length));
  status('chat-error', entries.findLast((entry) => entry.state === 'failed')?.error);
  container.replaceChildren(...entries.map((entry) => {
    const article = make('article', 'message human');
    const row = make('div', 'outgoing-row');
    if (entry.state !== 'sent') {
      const failed = entry.state === 'failed';
      const indicator = failed ? button(t('retrySend'), 'delivery-failed', () => send(entry.text, entry))
        : make('span', 'delivery-pending');
      if (failed) {
        indicator.disabled = byId('send').disabled;
        indicator.prepend(glyph('close'));
        indicator.title = entry.error || t('messageSendFailed');
      } else {
        indicator.setAttribute('role', 'img');
        indicator.setAttribute('aria-label', t('messageSending'));
        indicator.title = t('messageSending');
      }
      row.append(indicator);
    }
    row.append(messageContent(entry.text, true));
    article.append(make('div', 'message-meta', t('you')), row);
    return article;
  }));
}

function renderHistory(target, added, older) {
  const messages = byId('messages');
  const scroller = byId('history');
  const nearBottom = scroller.scrollHeight - scroller.clientHeight - scroller.scrollTop < 60;
  const height = scroller.scrollHeight;
  const top = scroller.scrollTop;
  const queue = outgoing.get(target.key);
  if (!older && matchOutgoing(queue, added)) {
    if (!queue.entries.length) outgoing.delete(target.key);
    paintOutgoing();
  }
  paintMessageSelection();
  if (!target.rows.size) {
    messages.replaceChildren(make('div', 'empty', t('noMessages')));
    return;
  }
  if (target.rows.size === added.length) messages.replaceChildren();
  const rows = added.map((row) => {
    const human = row.role === 'user';
    const element = make('article', human ? 'message human' : 'message');
    const meta = make('div', 'message-meta');
    const pick = make('input', 'message-pick');
    pick.type = 'checkbox';
    pick.setAttribute('aria-label', t('selectMessage'));
    const select = (value) => {
      if (target !== transcript || !target.selected) return;
      pick.checked = value;
      element.classList.toggle('picked', value);
      if (value) target.selected.add(row.id);
      else target.selected.delete(row.id);
      paintMessageSelection();
    };
    pick.addEventListener('change', () => select(pick.checked));
    const startSelection = () => {
      if (target !== transcript) return;
      if (!target.selected) {
        pushChatLayer({ messageSelection: target.key });
        target.selected = new Set();
      }
      select(true);
    };
    onHold(element, startSelection, (event) => {
      if (target === transcript) messageMenu(event, startSelection);
    });
    element.addEventListener('click', (event) => {
      if (target.selected && !event.target.closest('input')) {
        event.preventDefault();
        select(!pick.checked);
        return;
      }
      if (event.button || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
      const link = event.target.closest('a[href]');
      if (link && openLinkPreview(link)) event.preventDefault();
    });
    const sender = make('span', '', human ? t('you') : 'Codex');
    meta.append(...(human ? [sender, pick] : [pick, sender]));
    element.append(meta, messageContent(row.text, human));
    return element;
  });
  if (older) {
    messages.prepend(...rows);
    scroller.scrollTop = top + scroller.scrollHeight - height;
    if (target.messageAnchor && Math.abs(top - target.messageAnchor.scrollTop) < 1) {
      target.messageAnchor.scrollTop = scroller.scrollTop;
    }
  } else {
    messages.append(...rows);
    if (nearBottom) scroller.scrollTop = scroller.scrollHeight;
  }
}

async function send(text = chatInput.value, retry = null) {
  if (byId('send').disabled || !text.trim() || !active) return;
  if (!retry && active.chat && /^\/[a-z][a-z0-9-]*(?: +[^\x00-\x1f\x7f]*)?$/i.test(text.trim())) return runSlash(text);
  hideSuggestions();
  const item = active;
  const key = conversationKey();
  const requestedSettings = nextSettings.get(key);
  const previous = retry || outgoing.get(key)?.entries.findLast((entry) => entry.state === 'failed' && entry.text === text);
  if (previous && item.thread) await loadHistory();
  if (byId('send').disabled || conversationKey() !== key || route !== 'agents') return;
  const queue = outgoing.get(key) || { entries: [], matched: new Set() };
  if (previous && (previous.state !== 'failed' || !queue.entries.includes(previous))) return;
  exitMessageSelection();
  const entry = previous || { id: crypto.randomUUID(), key, text, match: matchingText(text) };
  entry.state = 'sending';
  entry.startedAt = Date.now();
  entry.after = item.thread && transcript?.loaded ? transcript.latest + 1 : null;
  if (previous) queue.entries.splice(queue.entries.indexOf(previous), 1);
  queue.entries.push(entry);
  outgoing.set(key, queue);
  cacheChat(deliveryStorage + entry.id, entry);
  if (!retry || drafts.get(key)?.retry === retry.id) {
    rememberDraft(key, '');
    chatInput.value = '';
  }
  const current = () => signedIn && route === 'agents' && conversationKey() === entry.key;
  submitting = true;
  paintComposer();
  paintOutgoing();
  const scroller = byId('history');
  scroller.scrollTop = scroller.scrollHeight;
  try {
    const { after, submittedAt, settingsApplied } = await api('/api/prompt', 'POST', {
      ...chatTarget(item), text, requestId: entry.id, settings: requestedSettings,
    });
    entry.state = 'sent';
    if (Number.isSafeInteger(after) && after >= 0) entry.after = after;
    if (Number.isFinite(submittedAt)) entry.startedAt = submittedAt;
    if (settingsApplied && requestedSettings && nextSettings.get(entry.key) === requestedSettings) {
      if (requestedSettings.model) chatModels.set(entry.key, requestedSettings);
      if (requestedSettings.permission) chatPermissions.set(entry.key, requestedSettings.permission);
      rememberSettings(entry.key, {});
    }
    const delivered = outgoing.get(entry.key);
    if (current()) {
      if (transcript?.key === entry.key) matchOutgoing(delivered, transcript.rows.values());
      if (delivered && !delivered.entries.length) outgoing.delete(entry.key);
      paintOutgoing();
      loadHistory();
    }
    if (delivered?.entries.includes(entry)) cacheChat(deliveryStorage + entry.id, entry);
  } catch (error) {
    if (!outgoing.get(entry.key)?.entries.includes(entry)) return;
    entry.state = 'failed';
    entry.error = error.message;
    cacheChat(deliveryStorage + entry.id, entry);
    if (!drafts.has(entry.key)) rememberDraft(entry.key, text, entry.id);
    if (current()) {
      const nearBottom = scroller.scrollHeight - scroller.clientHeight - scroller.scrollTop < 60;
      chatInput.value = drafts.get(entry.key)?.text || '';
      resizeComposer();
      paintOutgoing();
      if (nearBottom) scroller.scrollTop = scroller.scrollHeight;
    }
  } finally {
    submitting = false;
    if (supportsChat() && route === 'agents') {
      paintComposer();
      paintOutgoing();
    }
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

function terminalTheme() {
  const colors = getComputedStyle(document.documentElement);
  const color = (role) => colors.getPropertyValue('--' + role).trim();
  return {
    background: color('terminal'), foreground: color('terminal-ink'),
    cursor: color('terminal-caret'), cursorAccent: color('terminal'),
    ...Object.fromEntries(ansiNames.map((name, index) => [name, color('terminal-ansi-' + index)])),
  };
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
    cursorBlink: false, scrollback: 2000,
    theme: terminalTheme(),
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
    if (event.code === 4001) { requirePasskey(); return; }
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
    if (packet.submit && !packet.data.length) {
      terminal.socket.send(JSON.stringify(packet));
      return;
    }
    // Split paste into bounded chunks without bisecting Unicode code points.
    const characters = Array.from(packet.data);
    for (let offset = 0; offset < characters.length; offset += 2500) {
      terminal.socket.send(JSON.stringify({
        type: 'input', data: characters.slice(offset, offset + 2500).join(''),
        bracketed: packet.submit, submit: packet.submit && offset + 2500 >= characters.length,
      }));
    }
  } else terminal.socket.send(JSON.stringify(packet));
}

function sendKeys(data, submit = false) {
  if (!terminal?.writable) return;
  const control = terminal.ctrl && /^[a-z]$/i.test(data);
  const alt = terminal.alt;
  if (control) data = String.fromCharCode(data.toUpperCase().charCodeAt(0) - 64);
  if (alt) data = '\x1b' + data;
  sendTerminal({ type: 'input', data, submit: submit && !control && !alt });
  terminal.ctrl = terminal.alt = false;
  terminalControls('inputEnabled');
}

function choice(label, detail, action) {
  const row = button('', 'sheet-row', action);
  row.append(make('span', 'choice', label), make('small', '', detail));
  return row;
}

function setPicker(trigger, label, options, value, update) {
  const selected = options.find((option) => option.value === value) || options[0];
  trigger.type = 'button';
  trigger.value = selected?.value || '';
  trigger.disabled = !selected;
  trigger.setAttribute('aria-haspopup', 'dialog');
  trigger.setAttribute('aria-controls', 'picker-dialog');
  trigger.setAttribute('aria-label', selected ? label + ' · ' + selected.label : label);
  if (trigger.textContent !== (selected?.label || label)) {
    trigger.replaceChildren(make('span', 'picker-value', selected?.label || label), glyph('down'));
  }
  trigger.onclick = () => {
    if (pickerDialog.open) return;
    byId('picker-title').textContent = label;
    const list = byId('picker-options');
    list.replaceChildren();
    let group;
    for (const option of options) {
      if (option.group && option.group !== group) list.append(make('h3', 'subheading', option.group));
      group = option.group;
      const row = choice(option.label, '', () => {
        pickerDialog.close();
        if (option.value === trigger.value) return;
        setPicker(trigger, label, options, option.value, update);
        update?.(option.value);
      });
      row.setAttribute('aria-current', String(option.value === trigger.value));
      list.append(row);
    }
    pickerDialog.showModal();
    list.querySelector('[aria-current="true"]')?.focus();
  };
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
  const machine = make('button', 'picker-button');
  const session = make('button', 'picker-button');
  const summary = make('p', 'sheet-note');
  const submit = make('button', 'primary', t('newAgent'));
  submit.type = 'submit';
  const preferred = scope === 'all' ? active?.node : scope;
  const eligible = online.find((item) => item.sidebar?.launchTool && item.sessions?.some((entry) => entry.pane));
  const initialMachine = online.some(({ id }) => id === preferred) ? preferred : eligible?.id;
  const sessionLabel = make('label', '', t('tmuxSession'));
  sessionLabel.append(session);
  const machineLabel = make('label', '', t('machine'));
  machineLabel.append(machine);
  form.append(machineLabel, sessionLabel, summary, submit);
  function locations() {
    const node = online.find(({ id }) => id === machine.value);
    const sessions = (node?.sessions || []).filter((item) => item.pane);
    const initialSession = machine.value === active?.node ? active.pane?.session : undefined;
    setPicker(session, t('tmuxSession'), sessions.map((item) => ({ value: item.id, label: item.title })), initialSession);
    const tool = node?.sidebar?.launchTool;
    summary.textContent = !node ? t('noOnlineMachines') : !tool ? t('noAgentLauncher')
      : !sessions.length ? t('noSessions') : t('launchUsing', { tool: kindNames[tool] || tool });
    submit.disabled = !tool || !sessions.length;
  }
  setPicker(machine, t('machine'), online.map((item) => ({ value: item.id, label: item.name })), initialMachine, locations);
  locations();
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
  const key = recordKey(item);
  const selected = selectedPane.get(key);
  const anchor = item.windows?.flatMap((window) => window.panes).find((pane) => pane.key === selected) || item.pane;
  const form = make('form', 'workspace-form');
  const placement = make('button', 'picker-button');
  setPicker(placement, t('placement'), [
    { value: 'window', label: t('newWindow') }, { value: 'split', label: t('splitPane') },
  ], 'window');
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
      selectedPane.set(key, pane.key);
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
  if (!signedIn || reauthRequired || (syncing && !force)) return;
  const revision = ++refreshing;
  syncing = true;
  try {
    const query = new URLSearchParams([...hiddenStates].map((state) => ['hide', state]));
    const result = await api('/api/workspace' + (query.size ? '?' + query : ''));
    if (!signedIn || revision !== refreshing) return;
    const fingerprint = JSON.stringify(result);
    if (fingerprint !== overview || force) {
      promoteChats(result.nodes);
      machines = result.nodes;
      overview = fingerprint;
      if (isWorkspace()) paintWorkspace();
      else {
        paintPalette();
        if (route === 'machines') paintMachines();
      }
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
  restoreChats();
  authGeneration++;
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
  authGeneration++;
  reauthRequired = false;
  byId('reauth').hidden = true;
  signedIn = false;
  route = 'login';
  machines = [];
  overview = '';
  active = null;
  activeKey = '';
  transcript = null;
  selection.clear();
  selectedPane.clear();
  hiddenStates.clear();
  expandedTeams.clear();
  expandedSessions.clear();
  launching = null;
  drafts.clear();
  outgoing.clear();
  chatModels.clear();
  chatPermissions.clear();
  nextSettings.clear();
  suggestionCache.clear();
  stopping = null;
  byId('outgoing-messages').replaceChildren();
  activeLink = '';
  byId('link-pages').replaceChildren();
  linkTabs.clear();
  linkOpener = null;
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

function requirePasskey() {
  if (!signedIn || reauthRequired) return;
  reauthRequired = true;
  byId('reauth').hidden = false;
  if (route === 'agents' && supportsChat()) paintComposer();
}

byId('reauth').addEventListener('click', async (event) => {
  const trigger = event.currentTarget;
  if (trigger.disabled) return;
  trigger.disabled = true;
  try {
    await authenticate();
    if (!signedIn) return;
    authGeneration++;
    reauthRequired = false;
    trigger.hidden = true;
    if (route === 'agents' && supportsChat()) paintComposer();
    if (terminal && terminal.socket?.readyState !== WebSocket.OPEN) connectTerminal(terminal);
    refresh(true);
  } catch (error) { toast(error.message); }
  finally { trigger.disabled = false; }
});

byId('modules').addEventListener('click', switchModule);
byId('machine').addEventListener('click', switchMachine);
byId('new-agent').addEventListener('click', newAgentSheet);
byId('new-shell').addEventListener('click', shellSheet);
byId('back').addEventListener('click', back);
byId('chat-menu').addEventListener('click', () => {
  const machine = machines.find(({ id }) => id === active?.node);
  if (byId('chat-menu').hidden || !machine) return;
  const pinned = machine.pinnedAgents?.some((card) => card.pane?.window === active.pane.window);
  showRecordMenu(active, machine, pinned);
});
byId('close-link-panel').addEventListener('click', closeLinkPanel);
byId('link-tabs').addEventListener('keydown', (event) => {
  if (event.target.getAttribute('role') !== 'tab') return;
  const keys = [...linkTabs.keys()];
  const current = keys.indexOf(activeLink);
  const index = { ArrowLeft: (current + keys.length - 1) % keys.length,
    ArrowRight: (current + 1) % keys.length, Home: 0, End: keys.length - 1 }[event.key];
  if (!Number.isInteger(index)) return;
  event.preventDefault();
  selectLinkTab(keys[index]);
});
byId('mine-back').addEventListener('click', back);
byId('picker-close').addEventListener('click', () => pickerDialog.close());
pickerDialog.addEventListener('click', (event) => { if (event.target === pickerDialog) pickerDialog.close(); });
pickerDialog.addEventListener('keydown', (event) => {
  const options = [...pickerDialog.querySelectorAll('.sheet-row')];
  const current = options.indexOf(document.activeElement);
  const index = { ArrowUp: Math.max(0, current - 1), ArrowDown: Math.min(options.length - 1, current + 1),
    Home: 0, End: options.length - 1 }[event.key];
  if (!options[index]) return;
  event.preventDefault();
  options[index].focus();
});
byId('previous-message').addEventListener('click', () => jumpToUserMessage('previous'));
byId('next-message').addEventListener('click', () => jumpToUserMessage('next'));
byId('history').addEventListener('scroll', () => {
  loadEarlierAtTop();
  paintMessageNavigation();
}, { passive: true });
new ResizeObserver(paintMessageNavigation).observe(byId('history'));
byId('exit-selection').addEventListener('click', () => {
  if (transcript?.selected) exitMessageSelection();
});
byId('copy-selection').addEventListener('click', () => copyMessages(selectedMessages()));
byId('share-selection').addEventListener('click', shareMessages);
document.addEventListener('keydown', (event) => {
  if (route !== 'agents' || overlay.open || pickerDialog.open) return;
  if (event.key === 'Escape' && (transcript?.selected || activeLink)) {
    event.preventDefault();
    if (!exitMessageSelection()) closeLinkPanel();
  } else if (transcript?.selected && !event.repeat && (event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'c'
      && !event.target.closest?.('input:not([type="checkbox"]),textarea,[contenteditable]')
      && !window.getSelection()?.toString() && transcript.selected.size) {
    event.preventDefault();
    copyMessages(selectedMessages());
  }
});
byId('chat-form').addEventListener('submit', (event) => { event.preventDefault(); send(); });
byId('stop-agent').addEventListener('click', stopAgent);
byId('chat-settings').addEventListener('click', showChatSettings);
byId('chat-permission').addEventListener('click', showPermissionSettings);
byId('command-dismiss').addEventListener('click', () => { byId('command-result').hidden = true; });
byId('command-open-pane').addEventListener('click', () => {
  const item = active;
  const machine = machines.find(({ id }) => id === item?.node);
  const session = machine?.sessions?.find(({ id }) => id === item.pane.session);
  if (!session) return;
  const record = withMachine({ ...session, sessionRecord: true }, machine);
  navigate('sessions', machine.id);
  selectedPane.set(recordKey(record), item.pane.key);
  openRecord(record);
});
chatInput.addEventListener('input', () => {
  rememberDraft(conversationKey(), chatInput.value);
  if (route === 'agents' && supportsChat()) paintComposer();
  suggestionIndex = 0;
  updateSuggestions();
});
chatInput.addEventListener('focus', updateSuggestions);
chatInput.addEventListener('blur', hideSuggestions);
chatInput.addEventListener('compositionstart', () => { composingPrompt = true; hideSuggestions(); });
chatInput.addEventListener('compositionend', () => {
  composingPrompt = false;
  rememberDraft(conversationKey(), chatInput.value);
  updateSuggestions();
});
chatInput.addEventListener('keydown', (event) => {
  if (event.isComposing || byId('chat-suggestions').hidden) return;
  if (event.key === 'Escape') {
    event.preventDefault();
    event.stopPropagation();
    hideSuggestions();
  } else if (suggestionItems.length && ['Enter', 'Tab'].includes(event.key)) {
    event.preventDefault();
    chooseSuggestion(suggestionItems[suggestionIndex]);
  } else if (suggestionItems.length && ['ArrowUp', 'ArrowDown'].includes(event.key)) {
    event.preventDefault();
    const step = event.key === 'ArrowUp' ? -1 : 1;
    suggestionIndex = (suggestionIndex + step + suggestionItems.length) % suggestionItems.length;
    updateSuggestions();
    byId('suggestion-' + suggestionIndex)?.scrollIntoView({ block: 'nearest' });
  }
});
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
document.addEventListener('pointerdown', () => { suppressHoldClick = false; }, true);
document.addEventListener('click', (event) => {
  // The release can land on a dialog or a message moved by the selection UI.
  if (!suppressHoldClick || !event.detail) return;
  suppressHoldClick = false;
  event.preventDefault();
  event.stopImmediatePropagation();
}, true);
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
  try { await api('/api/logout', 'POST'); signedOut(); }
  catch (error) { if (error.status === 401) signedOut(); else if (signedIn) toast(error.message); }
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
  const sameChat = route === 'agents' && saved.route === route && saved.scope === scope
    && saved.opened === opened && saved.selected === activeKey;
  // The panel can be closed while the chat's selection entry is still active.
  if (sameChat && saved.linkPreview && !saved.messageSelection && !activeLink && clearMessageSelection()) {
    history.back();
    return;
  }
  const previewWasOpen = Boolean(activeLink);
  activeLink = saved.linkPreview ? activeLink || [...linkTabs.keys()].at(-1) || '' : '';
  if (!sameChat) {
    clearMessageSelection();
    route = saved.route === 'login' ? 'agents' : saved.route;
    scope = saved.scope;
    opened = saved.opened;
    minePage = saved.minePage;
    if (saved.selected) selection.set(route, saved.selected);
    paint();
  }
  if (route === 'agents' && transcript && saved.messageSelection === transcript.key) {
    transcript.selected ??= new Set();
    paintMessageSelection();
  } else clearMessageSelection();
  if (sameChat) {
    paintLinkPanel();
    if (previewWasOpen && !activeLink && linkOpener?.isConnected) linkOpener.focus({ preventScroll: true });
  }
  if (route === 'mine') { if (minePage === 'keys') loadKeys(); else loadUsage(); }
});
mobile.addEventListener('change', () => { if (isWorkspace()) paintWorkspace(); });
darkMode.addEventListener('change', () => {
  paint();
  if (terminal?.xterm) terminal.xterm.options.theme = terminalTheme();
});
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
  resizeComposer();
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

function watchAppUpdates() {
  const worker = navigator.serviceWorker;
  if (!worker) return;
  const updateToast = byId('update-toast');
  let dismissed;
  let available;
  function checkVersion() { worker.controller?.postMessage('workbench-version'); }
  worker.addEventListener('message', (event) => {
    const { type, version } = event.data || {};
    if (event.source !== worker.controller || type !== 'workbench-version' || typeof version !== 'string') return;
    available = version;
    updateToast.hidden = version === appVersion || version === dismissed;
  });
  byId('update-dismiss').addEventListener('click', () => {
    dismissed = available;
    updateToast.hidden = true;
  });
  byId('update-refresh').addEventListener('click', () => window.location.reload());
  worker.addEventListener('controllerchange', checkVersion);
  worker.register('/sw.js', { updateViaCache: 'none' }).then((registration) => {
    checkVersion();
    let lastCheck = Date.now();
    function check() {
      if (document.hidden || !navigator.onLine || Date.now() - lastCheck < 30_000) return;
      lastCheck = Date.now();
      registration.update().catch(() => {});
    }
    setInterval(check, 60_000);
    document.addEventListener('visibilitychange', check);
    window.addEventListener('online', check);
  }).catch(() => {});
}

watchAppUpdates();
api('/api/session').then(({ authenticated }) => {
  if (authenticated) signedOn();
  if (enrollment) registerSheet();
}).catch(() => status('login-error', connectionError));
setInterval(() => {
  if (document.hidden) return;
  refresh();
  if (signedIn && route === 'agents' && supportsChat() && (opened || !mobile.matches)) loadHistory();
}, 3000);
