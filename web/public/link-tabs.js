import { button, glyph, make } from './dom.js?v=WORKBENCH_CACHE_VERSION';

export function mountLinkTabs(slot, { t, selectTab, closeTab }) {
  const strip = slot.querySelector('#link-tabs');
  const trigger = slot.querySelector('#link-tab-trigger');
  const triggerLabel = trigger.querySelector('.picker-value');
  const picker = slot.querySelector('#link-tab-picker');
  const pickerOpen = () => picker.matches(':popover-open');
  let tabs = new Map();
  let selected = '';

  function closePicker() {
    if (!pickerOpen()) return false;
    picker.hidePopover();
    return true;
  }

  function focusCurrent() {
    const current = pickerOpen() ? picker.querySelector('[aria-current="true"]')
      : slot.hasAttribute('data-compact') ? trigger : strip.querySelector('[aria-selected="true"]');
    current?.focus();
  }

  function positionPicker() {
    const { left, bottom, width } = trigger.getBoundingClientRect();
    picker.style.setProperty('--picker-left', left + 'px');
    picker.style.setProperty('--picker-top', bottom + 4 + 'px');
    picker.style.setProperty('--picker-width', width + 'px');
  }

  function fit() {
    if (!strip.clientWidth) return;
    // The strip stays laid out, off the accessibility tree, in compact mode.
    const compact = strip.scrollWidth > strip.clientWidth;
    const changed = slot.hasAttribute('data-compact') !== compact;
    const hadFocus = changed && slot.contains(document.activeElement);
    slot.toggleAttribute('data-compact', compact);
    if (!compact) closePicker();
    else if (pickerOpen()) positionPicker();
    if (hadFocus) focusCurrent();
  }

  function rows(vertical) {
    return Array.from(tabs, ([key, tab]) => {
      const current = key === selected;
      const controls = make('div', 'link-tab');
      const title = button(tab.label, 'link-tab-title', () => {
        if (vertical) closePicker();
        selectTab(key);
      });
      title.title = tab.url?.href || tab.source.path;
      if (vertical) {
        title.setAttribute('aria-current', String(current));
      } else {
        controls.setAttribute('role', 'presentation');
        title.id = tab.id + '-tab';
        title.setAttribute('role', 'tab');
        title.setAttribute('aria-selected', String(current));
        title.tabIndex = current ? 0 : -1;
      }
      if (tab.page) title.setAttribute('aria-controls', tab.id);
      const remove = button('', 'link-tab-close', () => closeTab(key));
      remove.title = t('closeLinkTab', { page: tab.label });
      remove.setAttribute('aria-label', remove.title);
      remove.append(glyph('close'));
      controls.append(title, remove);
      return controls;
    });
  }

  function move(event, previous, next, activate) {
    const buttons = [...event.currentTarget.querySelectorAll('.link-tab-title')];
    const index = buttons.indexOf(event.target);
    if (index < 0) return;
    const target = {
      [previous]: (index + buttons.length - 1) % buttons.length,
      [next]: (index + 1) % buttons.length,
      Home: 0,
      End: buttons.length - 1,
    }[event.key];
    if (!Number.isInteger(target)) return;
    event.preventDefault();
    if (activate) buttons[target].click();
    else buttons[target].focus();
  }

  picker.addEventListener('beforetoggle', (event) => {
    if (event.newState !== 'open') return;
    picker.replaceChildren(...rows(true));
    positionPicker();
  });
  picker.addEventListener('toggle', (event) => {
    const open = event.newState === 'open';
    trigger.setAttribute('aria-expanded', String(open));
    if (open) focusCurrent();
  });
  strip.addEventListener('keydown', (event) => move(event, 'ArrowLeft', 'ArrowRight', true));
  picker.addEventListener('keydown', (event) => move(event, 'ArrowUp', 'ArrowDown', false));
  new ResizeObserver(fit).observe(slot);

  return {
    closePicker,
    focusCurrent,
    render(entries, current) {
      tabs = entries;
      selected = current;
      const label = tabs.get(current)?.label || '';
      triggerLabel.textContent = label;
      trigger.title = label;
      trigger.setAttribute('aria-label', t('linkTabs') + ' · ' + label);
      strip.replaceChildren(...rows(false));
      fit();
      if (pickerOpen()) picker.replaceChildren(...rows(true));
    },
  };
}
