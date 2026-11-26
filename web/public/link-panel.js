import { bindBackButton } from './dom.js?v=WORKBENCH_CACHE_VERSION';
import { workspaceLink } from './link-policy.js?v=WORKBENCH_CACHE_VERSION';
import { createLinkPage, linkPreviewSource, syncFrameAppearance } from './link-preview.js?v=WORKBENCH_CACHE_VERSION';
import { mountLinkTabs } from './link-tabs.js?v=WORKBENCH_CACHE_VERSION';
import { refreshDiagramFigures } from './mermaid-preview.js?v=WORKBENCH_CACHE_VERSION';

export function mountLinkPanel(panel, toggle, { t, markdown, fileMarkdown, getMode, readFile, fullScreen, onVisibilityChange, onLayoutChange, onEscape }) {
  const backButton = panel.querySelector('#back-link-panel');
  const closeButton = panel.querySelector('#close-link-panel');
  const openInBrowser = panel.querySelector('#open-link-browser');
  const tabBar = mountLinkTabs(panel.querySelector('.link-tab-slot'), { t, selectTab, closeTab });
  const pages = panel.querySelector('#link-pages');
  const empty = panel.querySelector('#link-preview-empty');
  const tabs = new Map();
  let selected = '';
  let visible = false;
  let opener;
  let transition;
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
    if (!open && !visible && transition && returnFocus) return;
    const wasVisible = visible;
    visible = open;
    transition?.cancel();
    transition = undefined;
    panel.inert = false;
    if (!visible) tabBar.closePicker();
    toggle.setAttribute('aria-expanded', String(visible));
    toggle.title = t(visible ? 'closeLinkPreview' : 'openLinkPreview');
    toggle.setAttribute('aria-label', toggle.title);
    const animate = document.documentElement.dataset.motion === 'full';
    const rest = { opacity: 1, transform: 'translateX(0)' };
    const away = { opacity: 0, transform: 'translateX(8px)' };
    if (visible) {
      panel.hidden = false;
      onLayoutChange();
      renderTabs();
      if (!wasVisible && fullScreen.matches) backButton.focus({ preventScroll: true });
      if (!wasVisible && animate) transition = panel.animate([away, rest], { duration: 145, easing: 'ease-out' });
    } else {
      function hide() {
        panel.hidden = true;
        panel.inert = false;
        onLayoutChange();
        if (wasVisible && returnFocus) (opener?.isConnected ? opener : toggle).focus({ preventScroll: true });
      }
      if (wasVisible && returnFocus && animate) {
        panel.inert = true;
        const closing = panel.animate([rest, away], { duration: 130, easing: 'ease-in', fill: 'forwards' });
        transition = closing;
        closing.finished.then(() => {
          if (transition !== closing) return;
          transition = undefined;
          closing.cancel();
          hide();
        }).catch(() => {});
      } else if (!panel.hidden) hide();
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

  function openTab(key, label, source, url, trigger) {
    if (!tabs.has(key)) tabs.set(key, { id: 'link-preview-' + ++nextId, label, url, source, page: null });
    if (trigger.ownerDocument === panel.ownerDocument && !panel.contains(trigger)) opener = trigger;
    selected = key;
    show();
    return true;
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
    const label = link.textContent.trim() || local?.path.split('/').at(-1) || url?.hostname;
    return openTab(key, label, source, url, link);
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
  fullScreen.addEventListener('change', () => {
    if (!visible) return;
    if (fullScreen.matches && (!panel.contains(document.activeElement) || document.activeElement === closeButton)) backButton.focus();
    else if (!fullScreen.matches && document.activeElement === backButton) closeButton.focus();
  });
  return {
    get isOpen() { return visible; },
    openLink,
    openDiagram(text, trigger) {
      return openTab('diagram:' + text, t('mermaidDiagram'), { kind: 'diagram', text }, undefined, trigger);
    },
    close,
    closeTabPicker: tabBar.closePicker,
    restore,
    updateAppearance(themeChanged) {
      if (themeChanged) refreshDiagramFigures(pages);
      for (const frame of pages.querySelectorAll('iframe.link-frame[srcdoc]')) {
        syncFrameAppearance(frame.contentDocument);
        if (themeChanged && frame.contentDocument) refreshDiagramFigures(frame.contentDocument);
      }
    },
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
