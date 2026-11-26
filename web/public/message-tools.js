import { glyph, make } from './dom.js?v=WORKBENCH_CACHE_VERSION';

const states = { running: 'toolRunning', finished: 'toolFinished', unconfirmed: 'toolUnconfirmed' };

export function paintMessageTools(message, tools, t, language) {
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
  const entries = tools.map(({ name, state, elapsed }) => {
    const row = make('li');
    const label = Object.hasOwn(states, state) ? states[state] : 'toolUnconfirmed';
    const text = [t(label), Number.isFinite(elapsed) && seconds.format(elapsed / 1000)].filter(Boolean).join(' · ');
    row.append(make('span', 'tool-name', name), make('span', 'tool-state', text));
    return row;
  });
  disclosure.querySelector('ul').replaceChildren(...entries);
}
