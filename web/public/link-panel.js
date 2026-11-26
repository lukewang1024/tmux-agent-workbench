import { button, glyph, make } from './dom.js?v=WORKBENCH_CACHE_VERSION';
import { createLinkPage } from './link-preview.js?v=WORKBENCH_CACHE_VERSION';

export function mountLinkPanel(panel, toggle, { t, markdown, onVisibilityChange }) {
  const closeButton = panel.querySelector('#close-link-panel');
  const openInBrowser = panel.querySelector('#open-link-browser');
  const tabList = panel.querySelector('#link-tabs');
  const pages = panel.querySelector('#link-pages');
  const empty = panel.querySelector('#link-preview-empty');
  const fullScreen = window.matchMedia('(max-width: 1023px)');
  const tabs = new Map();
  let selected = '';
  let visible = false;
  let opener;
  let nextId = 0;

  function renderTabs() {
    empty.hidden = tabs.size !== 0;
    openInBrowser.hidden = !selected;
    if (selected) openInBrowser.href = selected;
    tabList.replaceChildren(...Array.from(tabs, ([url, tab]) => {
      const current = url === selected;
      if (current && !tab.page) {
        const page = createLinkPage(new URL(url), tab.label, { t, markdown, openLink });
        page.id = tab.id;
        page.setAttribute('role', 'tabpanel');
        page.setAttribute('aria-labelledby', tab.id + '-tab');
        page.tabIndex = 0;
        pages.append(page);
        tab.page = page;
      }
      if (tab.page) tab.page.hidden = !current;
      const controls = make('div', 'link-tab');
      controls.setAttribute('role', 'presentation');
      const title = button(tab.label, 'link-tab-title', () => selectTab(url));
      title.id = tab.id + '-tab';
      title.title = url;
      title.setAttribute('role', 'tab');
      title.setAttribute('aria-selected', String(current));
      title.tabIndex = current ? 0 : -1;
      if (tab.page) title.setAttribute('aria-controls', tab.id);
      const closeTabButton = button('', 'link-tab-close', () => closeTab(url));
      closeTabButton.title = t('closeLinkTab', { page: tab.label });
      closeTabButton.setAttribute('aria-label', closeTabButton.title);
      closeTabButton.append(glyph('close'));
      controls.append(title, closeTabButton);
      return controls;
    }));
    const current = tabList.querySelector('[aria-selected="true"]');
    if (current && (current.offsetLeft < tabList.scrollLeft
        || current.offsetLeft + current.offsetWidth > tabList.scrollLeft + tabList.clientWidth)) {
      current.scrollIntoView({ block: 'nearest', inline: 'nearest' });
    }
  }

  function restore(open, returnFocus = false) {
    const wasVisible = visible;
    visible = open;
    panel.hidden = !visible;
    toggle.setAttribute('aria-expanded', String(visible));
    toggle.title = t(visible ? 'closeLinkPreview' : 'openLinkPreview');
    toggle.setAttribute('aria-label', toggle.title);
    if (visible) {
      renderTabs();
      if (!wasVisible && fullScreen.matches) closeButton.focus({ preventScroll: true });
    } else if (wasVisible && returnFocus) {
      (opener?.isConnected ? opener : toggle).focus({ preventScroll: true });
    }
  }

  function show() {
    if (!visible) onVisibilityChange(true);
    restore(true);
  }

  function close() {
    if (!visible) return;
    restore(false, true);
    onVisibilityChange(false);
  }

  function openLink(link) {
    const url = new URL(link.href);
    if (!['http:', 'https:'].includes(url.protocol) || url.origin === location.origin) return false;
    if (!tabs.has(url.href)) {
      const label = link.textContent.trim() || url.hostname;
      tabs.set(url.href, { id: 'link-preview-' + ++nextId, label, page: null });
    }
    if (!panel.contains(link)) opener = link;
    selected = url.href;
    show();
    return true;
  }

  function selectTab(url) {
    selected = url;
    renderTabs();
    tabList.querySelector('[aria-selected="true"]')?.focus();
  }

  function closeTab(url) {
    const keys = [...tabs.keys()];
    const index = keys.indexOf(url);
    tabs.get(url).page?.remove();
    tabs.delete(url);
    if (selected === url) selected = keys[index + 1] || keys[index - 1] || '';
    if (selected) selectTab(selected);
    else close();
  }

  toggle.addEventListener('click', () => {
    opener = toggle;
    if (visible) close();
    else show();
  });
  closeButton.addEventListener('click', close);
  tabList.addEventListener('keydown', (event) => {
    if (event.target.getAttribute('role') !== 'tab') return;
    const keys = [...tabs.keys()];
    const current = keys.indexOf(selected);
    const index = { ArrowLeft: (current + keys.length - 1) % keys.length,
      ArrowRight: (current + 1) % keys.length, Home: 0, End: keys.length - 1 }[event.key];
    if (!Number.isInteger(index)) return;
    event.preventDefault();
    selectTab(keys[index]);
  });
  return {
    get isOpen() { return visible; },
    openLink,
    close,
    restore,
    reset() {
      restore(false);
      for (const tab of tabs.values()) tab.page?.remove();
      tabs.clear();
      tabList.replaceChildren();
      selected = '';
      opener = null;
    },
  };
}
