import { button, glyph, make } from './dom.js?v=WORKBENCH_CACHE_VERSION';

const keyFor = (node, id, pane) => JSON.stringify([node, id, pane]);

export class DoneNotifications {
  #observed = new Map();
  #items = new Map();

  observe(nodes) {
    const added = [];
    for (const node of nodes) {
      if (node.state !== 'online' || !node.snapshotAvailable) continue;
      const previous = this.#observed.get(node.id);
      const current = new Map();
      for (const agent of node.agents || []) {
        if (!agent.pane?.key || agent.exited) continue;
        const key = keyFor(node.id, agent.id, agent.pane.key);
        current.set(key, agent.state);
        if (previous?.get(key) !== 'working' || agent.state !== 'done') continue;
        const item = { key, node: node.id, machine: node.name, id: agent.id, pane: agent.pane.key, title: agent.title };
        this.#items.set(key, item);
        added.push(key);
      }
      for (const key of previous?.keys() || []) {
        if (current.get(key) !== 'done') this.#items.delete(key);
      }
      this.#observed.set(node.id, current);
    }
    const available = new Set(nodes.map(({ id }) => id));
    for (const [node, previous] of this.#observed) {
      if (available.has(node)) continue;
      for (const key of previous.keys()) this.#items.delete(key);
      this.#observed.delete(node);
    }
    return added;
  }

  get items() { return [...this.#items.values()].reverse(); }
  get(key) { return this.#items.get(key); }
  read(node, id, pane) { return this.#items.delete(keyFor(node, id, pane)); }
  clear() { this.#items.clear(); }
  reset() { this.clear(); this.#observed.clear(); }
}

export function mountDoneNotifications(root, { t, openAgent }) {
  const byId = (id) => root.querySelector('#' + id);
  const trigger = byId('done-trigger');
  const badge = byId('done-count');
  const popover = byId('done-popover');
  const list = byId('done-list');
  const clear = byId('done-clear');
  const banner = byId('done-toast');
  const bannerTitle = byId('done-toast-title');
  const bannerOpen = byId('done-toast-open');
  const state = new DoneNotifications();
  const queue = [];
  let current;
  let timeout;
  let rendered;

  function showNext() {
    if (current) return;
    while (queue.length) {
      const item = state.get(queue.shift());
      if (!item) continue;
      current = item;
      bannerTitle.textContent = t('agentFinished', { agent: item.title });
      bannerOpen.setAttribute('aria-label', bannerTitle.textContent);
      banner.hidden = false;
      timeout = setTimeout(dismissToast, 7000);
      break;
    }
  }

  function dismissToast() {
    clearTimeout(timeout);
    banner.hidden = true;
    current = undefined;
    showNext();
  }

  function activate(item) {
    if (!item) return;
    if (popover.matches(':popover-open')) popover.hidePopover();
    openAgent(item);
  }

  function render() {
    const items = state.items;
    const revision = JSON.stringify(items);
    if (revision === rendered) return;
    rendered = revision;
    badge.hidden = !items.length;
    badge.textContent = items.length;
    trigger.setAttribute('aria-label', items.length ? t('completionCount', { count: items.length }) : t('completionNotifications'));
    clear.disabled = !items.length;
    list.replaceChildren(...items.map((item) => {
      const row = button('', 'done-row', () => activate(item));
      const copy = make('span', 'row-copy');
      copy.append(make('b', '', item.title), make('small', '', item.machine));
      row.append(copy, glyph('next'));
      return row;
    }));
    if (!items.length) list.append(make('p', 'empty', t('noCompletionNotifications')));
    if (current && !state.get(current.key)) dismissToast();
    else showNext();
  }

  function hide() {
    if (popover.matches(':popover-open')) popover.hidePopover();
  }

  popover.addEventListener('toggle', (event) => trigger.setAttribute('aria-expanded', String(event.newState === 'open')));
  bannerOpen.addEventListener('click', () => activate(current));
  byId('done-toast-dismiss').addEventListener('click', dismissToast);
  clear.addEventListener('click', () => { state.clear(); queue.length = 0; render(); });
  render();
  return {
    observe(nodes) { queue.push(...state.observe(nodes)); render(); },
    read(node, id, pane) { if (state.read(node, id, pane)) render(); },
    hide,
    reset() { state.reset(); queue.length = 0; hide(); render(); },
  };
}
