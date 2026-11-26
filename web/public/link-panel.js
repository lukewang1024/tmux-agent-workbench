import { bindBackButton } from './dom.js?v=WORKBENCH_CACHE_VERSION';
import { workspaceLink } from './link-policy.js?v=WORKBENCH_CACHE_VERSION';
import { createLinkPage, linkPreviewSource } from './link-preview.js?v=WORKBENCH_CACHE_VERSION';
import { mountLinkTabs } from './link-tabs.js?v=WORKBENCH_CACHE_VERSION';

export function mountLinkPanel(panel, toggle, { t, markdown, fileMarkdown, getMode, readFile, onVisibilityChange, onEscape }) {
  const backButton = panel.querySelector('#back-link-panel');
  const closeButton = panel.querySelector('#close-link-panel');
  const openInBrowser = panel.querySelector('#open-link-browser');
  const tabBar = mountLinkTabs(panel.querySelector('.link-tab-slot'), { t, selectTab, closeTab });
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
    const selectedTab = tabs.get(selected);
    openInBrowser.hidden = !selectedTab?.url;
    if (selectedTab?.url) openInBrowser.href = selectedTab.url.href;
    else openInBrowser.removeAttribute('href');
    for (const [key, tab] of tabs) {
      const current = key === selected;
      if (current && !tab.page) {
        const page = createLinkPage(tab.url, tab.source, { t, markdown, fileMarkdown, readFile, openLink, onEscape, label: tab.label });
        page.id = tab.id;
        page.setAttribute('role', 'tabpanel');
        page.setAttribute('aria-labelledby', tab.id + '-tab');
        page.tabIndex = 0;
        pages.append(page);
        tab.page = page;
      }
      if (tab.page) tab.page.hidden = !current;
    }
    tabBar.render(tabs, selected);
  }

  function restore(open, returnFocus = false) {
    const wasVisible = visible;
    visible = open;
    if (!visible) tabBar.closePicker();
    panel.hidden = !visible;
    toggle.setAttribute('aria-expanded', String(visible));
    toggle.title = t(visible ? 'closeLinkPreview' : 'openLinkPreview');
    toggle.setAttribute('aria-label', toggle.title);
    if (visible) {
      renderTabs();
      if (!wasVisible && fullScreen.matches) backButton.focus({ preventScroll: true });
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

  function openLink(link, target) {
    const local = target && workspaceLink(link.getAttribute('href'));
    let source, url, key;
    if (local) {
      source = { kind: 'file', target, ...local };
      key = JSON.stringify([target.node, target.pane, target.thread, local.path, local.line]);
    } else {
      try { url = new URL(link.href); } catch { return false; }
      source = linkPreviewSource(url, getMode());
      if (!source || url.origin === location.origin) return false;
      key = source.kind + ':' + url.href;
    }
    if (!tabs.has(key)) {
      const label = link.textContent.trim() || local?.path.split('/').at(-1) || url?.hostname;
      tabs.set(key, { id: 'link-preview-' + ++nextId, label, url, source, page: null });
    }
    if (link.ownerDocument === panel.ownerDocument && !panel.contains(link)) opener = link;
    selected = key;
    show();
    return true;
  }

  function selectTab(url) {
    selected = url;
    renderTabs();
    tabBar.focusCurrent();
  }

  function closeTab(url) {
    const keys = [...tabs.keys()];
    const index = keys.indexOf(url);
    tabs.get(url).page?.remove();
    tabs.delete(url);
    if (selected === url) selected = keys[index + 1] || keys[index - 1] || '';
    if (selected) selectTab(selected);
    else {
      renderTabs();
      close();
    }
  }

  toggle.addEventListener('click', () => {
    opener = toggle;
    if (visible) close();
    else show();
  });
  bindBackButton(backButton, close);
  closeButton.addEventListener('click', close);
  return {
    get isOpen() { return visible; },
    openLink,
    close,
    closeTabPicker: tabBar.closePicker,
    restore,
    reset() {
      restore(false);
      for (const tab of tabs.values()) tab.page?.remove();
      tabs.clear();
      selected = '';
      renderTabs();
      opener = null;
    },
  };
}
