import { button, glyph, make } from './dom.js?v=WORKBENCH_CACHE_VERSION';

const states = { running: 'toolRunning', finished: 'toolFinished', unconfirmed: 'toolUnconfirmed' };

export function runningToolActivity({ messages, runningTools, runningToolNames }) {
  if (!Number.isSafeInteger(runningTools) || runningTools <= 0) return undefined;
  // Older connectors may only attach tool names to individual messages.
  const names = Array.isArray(runningToolNames) ? runningToolNames : messages.flatMap(({ tools = [] }) =>
    tools.filter(({ state }) => state === 'running').map(({ name }) => name));
  return { count: runningTools, names };
}

export function mountRunningTools(trigger, { t, sheet }) {
  const label = trigger.querySelector('span');
  const list = make('ul', 'running-tool-list');
  const note = make('p', 'sheet-note');
  let revision;

  trigger.addEventListener('click', () => sheet(t('runningTools'), list, note));
  return ({ count = 0, names = [] } = {}) => {
    const next = JSON.stringify([count, names]);
    if (revision === next) return;
    revision = next;
    trigger.hidden = !count;
    label.textContent = count ? t('runningToolCount', { count }) : '';
    list.hidden = !count || !names.length;
    list.replaceChildren(...names.map((name) => make('li', 'tool-name', name)));
    note.hidden = count > 0 && names.length >= count;
    if (!note.hidden) note.textContent = t(count ? 'runningToolNamesUnavailable' : 'noRunningTools');
  };
}

export function mountToolDetails(popover, t) {
  const text = popover.querySelector('pre');
  const note = popover.querySelector('p');
  let source;
  const isOpen = () => popover.matches(':popover-open');
  function place() {
    if (!source?.isConnected || !source.getClientRects().length) return close();
    const { left, top, bottom } = source.getBoundingClientRect();
    const width = Math.min(560, innerWidth - 16);
    const below = innerHeight - bottom - 12;
    const above = top - 12;
    const flip = below < 200 && above > below;
    popover.style.inlineSize = width + 'px';
    popover.style.maxBlockSize = Math.max(0, Math.min(340, flip ? above : below)) + 'px';
    popover.style.left = Math.min(innerWidth - width - 8, Math.max(8, left)) + 'px';
    popover.style.top = flip ? 'auto' : bottom + 5 + 'px';
    popover.style.bottom = flip ? innerHeight - top + 5 + 'px' : 'auto';
  }
  function close() {
    if (!isOpen()) return false;
    popover.hidePopover();
    return true;
  }
  popover.addEventListener('toggle', () => {
    if (isOpen()) return;
    source?.setAttribute('aria-expanded', 'false');
    source = undefined;
    text.textContent = '';
  });
  window.addEventListener('resize', () => { if (isOpen()) place(); });
  window.addEventListener('scroll', ({ target }) => {
    if (isOpen() && target.contains?.(source)) place();
  }, true);
  return {
    open(trigger, invocation, truncated) {
      if (source === trigger && close()) return;
      source?.setAttribute('aria-expanded', 'false');
      source = trigger;
      text.textContent = invocation;
      note.hidden = !truncated;
      note.textContent = truncated ? t('toolInvocationTruncated') : '';
      place();
      if (!isOpen()) popover.showPopover({ source: trigger });
      trigger.setAttribute('aria-expanded', 'true');
    },
    close,
  };
}

export function paintMessageTools(message, tools, t, language, toolDetails) {
  if (!Array.isArray(tools) || !tools.length) return;
  let disclosure = message.querySelector('.message-tools');
  const revision = JSON.stringify(tools);
  if (disclosure?.dataset.revision === revision) return;
  if (!disclosure) {
    disclosure = make('details', 'message-tools');
    const summary = make('summary');
    summary.append(make('span'), glyph('down'));
    disclosure.append(summary, make('ul'));
    message.append(disclosure);
  }
  disclosure.dataset.revision = revision;
  disclosure.querySelector('summary span').textContent = t('messageToolCount', { count: tools.length });
  const seconds = new Intl.NumberFormat(language, { style: 'unit', unit: 'second', maximumFractionDigits: 1 });
  const entries = tools.map(({ name, state, elapsed, invocation, invocationTruncated }) => {
    const row = make('li');
    const label = Object.hasOwn(states, state) ? states[state] : 'toolUnconfirmed';
    const text = [t(label), Number.isFinite(elapsed) && seconds.format(elapsed / 1000)].filter(Boolean).join(' · ');
    const nameLabel = make('span', 'tool-name', name);
    const stateLabel = make('span', 'tool-state', text);
    if (typeof invocation !== 'string' || !invocation) {
      row.append(nameLabel, stateLabel);
      return row;
    }
    const trigger = button('', 'tool-detail-trigger', (event) => {
      event.stopPropagation();
      toolDetails.open(trigger, invocation, invocationTruncated);
    });
    trigger.setAttribute('aria-controls', 'tool-detail');
    trigger.setAttribute('aria-expanded', 'false');
    trigger.append(nameLabel, stateLabel, glyph('next'));
    row.append(trigger);
    return row;
  });
  disclosure.querySelector('ul').replaceChildren(...entries);
}
