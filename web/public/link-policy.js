export const linkModes = ['balanced', 'browser', 'iframe'];
const preferenceKey = 'workbench.link-opening';
export const webDevMirror = 'web.developers.google.cn';

export function linkPreference() {
  try {
    const mode = localStorage.getItem(preferenceKey);
    if (linkModes.includes(mode)) return mode;
  } catch { /* Storage may be disabled. */ }
  return 'balanced';
}

export function saveLinkPreference(mode) {
  try { localStorage.setItem(preferenceKey, mode); } catch { /* Keep the current preference for this page. */ }
}

export function readerURL(url) {
  if (!['https://web.dev', 'https://' + webDevMirror].includes(url.origin) || url.username || url.password
      || url.pathname.length > 1024 || !/^\/(?:articles|learn)(?:\/[a-z\d_-]+)+\/?$/i.test(url.pathname)) return null;
  const result = new URL(url.pathname, url.origin);
  const language = url.searchParams.get('hl');
  if (/^[a-z]{2,3}(?:-[a-z\d]{2,8})*$/i.test(language || '')) result.searchParams.set('hl', language);
  return result;
}

export function workspaceLink(href) {
  if (typeof href !== 'string' || !href || href.length > 8192 || /^[?#]|^\/\//.test(href)) return null;
  let address = href;
  if (/^[a-z][a-z\d+.-]*:/i.test(href)) {
    // A relative filename with a line suffix is also a valid Workbench link.
    if (!/^file:/i.test(href)) {
      if (!/^[^:/?#]+:\d+(?::\d+)?(?:#.*)?$/.test(href)) return null;
    } else {
      let url;
      try { url = new URL(href); } catch { return null; }
      if (url.hostname || url.search || !url.pathname.startsWith('/')) return null;
      address = url.pathname + url.hash;
    }
  }
  const [encoded, fragment] = address.split('#', 2);
  if (!encoded || encoded.includes('?')) return null;
  const suffix = /:(\d+)(?::\d+)?$/.exec(encoded);
  const line = Number(/^L(\d+)(?:-L?\d+)?$/i.exec(fragment || '')?.[1] || suffix?.[1]) || null;
  let path;
  try { path = decodeURIComponent(suffix ? encoded.slice(0, suffix.index) : encoded); } catch { return null; }
  if (!path || /[\x00-\x1f\x7f\\]/.test(path) || path.startsWith('//')) return null;
  return { path, line: Number.isSafeInteger(line) && line > 0 ? line : null };
}
