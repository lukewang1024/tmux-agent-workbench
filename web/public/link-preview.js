import { renderWorkspaceFile } from './file-preview.js?v=WORKBENCH_CACHE_VERSION';
import { githubMarkdownURL, readerURL } from './link-policy.js?v=WORKBENCH_CACHE_VERSION';
import { diagramFigure, renderMarkdownDiagrams } from './mermaid-preview.js?v=WORKBENCH_CACHE_VERSION';
import { readerArticle } from './reader-preview.js?v=WORKBENCH_CACHE_VERSION';

const githubOrigin = 'https://github.com';
const docsOrigin = 'https://docs.github.com';
const mdnOrigin = 'https://developer.mozilla.org';
const rawOrigin = 'https://raw.githubusercontent.com';
const androidOrigin = 'https://developer.android.com';
const androidMirror = 'https://developer.android.google.cn';
const githubImages = docsOrigin + '/assets/images/';
const githubSections = new Set(['apps', 'collections', 'features', 'marketplace', 'orgs', 'settings', 'topics']);
const filesPerPage = 30;
const maxDiffPages = 3000 / filesPerPage;
const temporaryStatuses = new Set([408, 500, 502, 503, 504]);

function githubMarkdownEndpoint(path) {
  return '/api/github-markdown?' + new URLSearchParams({ path });
}

function githubSource(path) {
  const raw = githubMarkdownURL(path);
  if (raw) {
    const repository = raw.pathname.split('/').slice(1, 3).join('/');
    return { kind: 'article', endpoint: githubMarkdownEndpoint(path),
      mime: 'text/plain', imageRoot: raw.origin + '/' + repository + '/', raw: raw.href };
  }
  const match = /^\/([a-z\d-]{1,39})\/([a-z\d_.-]{1,100})(?:\/pull\/([1-9]\d{0,14})\/files)?\/?$/i.exec(path);
  if (!match) return null;
  const [, owner, repo, pull] = match;
  if (githubSections.has(owner.toLowerCase())) return null;
  const endpoint = 'https://api.github.com/repos/' + owner + '/' + repo;
  if (!pull) return { kind: 'readme', endpoint: endpoint + '/readme' };
  return { kind: 'diff', label: owner + '/' + repo + ' #' + pull,
    endpoint: endpoint + '/pulls/' + pull + '/files' };
}

function structuredSource(url) {
  if (url.protocol !== 'https:' || url.port) return null;
  const reader = readerURL(url);
  if (reader) return { kind: 'reader', endpoint: '/api/reader-source?' + new URLSearchParams({ url: reader.href }) };
  if (url.origin === githubOrigin) return githubSource(url.pathname);
  if (url.origin === docsOrigin) {
    if (!/^\/[a-z]{2}(?:-[a-z]{2})?(?:\/[a-z\d][a-z\d.@_-]*){2,}\/?$/i.test(url.pathname)) return null;
    return { kind: 'article', endpoint: docsOrigin + '/api/article/body?' +
      new URLSearchParams({ pathname: url.pathname.replace(/\/$/, '') }),
    mime: 'text/markdown', imageRoot: githubImages };
  }
  if (url.origin === androidOrigin || url.origin === androidMirror) {
    const endpoint = '/api/android-doc?' + new URLSearchParams({ path: url.pathname });
    return { kind: 'article', endpoint, mime: 'text/markdown', imageRoot: androidMirror + '/static/', android: true };
  }
  if (url.origin !== mdnOrigin) return null;
  const match = /^\/([a-z]{2}(?:-[a-z]{2})?)\/docs\/([a-z\d_-]+(?:\/[a-z\d_.-]+)*)\/?$/i.exec(url.pathname);
  if (!match) return null;
  const [, language, slug] = match.map((part) => part.toLowerCase());
  const repo = language === 'en-us' ? 'content' : 'translated-content';
  const imageRoot = rawOrigin + '/mdn/' + repo + '/main/files/';
  return { kind: 'article', endpoint: imageRoot + language + '/' + slug + '/index.md',
    mime: 'text/plain', imageRoot, mdn: true };
}

export function linkPreviewSource(url, mode = 'balanced') {
  if (mode === 'browser' || url.username || url.password || !['https:', 'http:'].includes(url.protocol)) return null;
  return structuredSource(url) || (mode === 'iframe' ? { kind: 'iframe' } : null);
}

function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function action(label, click) {
  const button = element('button', 'secondary link-load-button', label);
  button.type = 'button';
  button.addEventListener('click', click);
  return button;
}

class PreviewError extends Error {}

export function previewResponseError(response, mime, endpoint) {
  if (response.ok && response.headers.get('content-type')?.split(';', 1)[0] === mime) return null;
  const github = String(endpoint).startsWith('https://api.github.com/');
  if (response.status === 429 || (github && response.status === 403
      && (response.headers.get('x-ratelimit-remaining') === '0' || response.headers.has('retry-after')))) {
    return 'linkPreviewRateLimit';
  }
  if (temporaryStatuses.has(response.status)) return 'linkPreviewTemporary';
  return 'linkPreviewUnavailable';
}

async function publicContent(endpoint, accept, mime = accept) {
  let response;
  try {
    response = await fetch(endpoint, {
      credentials: typeof endpoint === 'string' && endpoint.startsWith('/api/') ? 'same-origin' : 'omit',
      redirect: 'error',
      referrerPolicy: 'no-referrer',
      headers: { accept },
      signal: AbortSignal.timeout(10_000),
    });
  } catch (error) {
    throw new PreviewError(error.name === 'TimeoutError' ? 'linkPreviewTemporary' : 'linkPreviewUnavailable');
  }
  const error = previewResponseError(response, mime, endpoint);
  if (error) throw new PreviewError(error);
  return response;
}

async function loadContent(status, read, t, url) {
  status.textContent = t('loadingLinkPreview');
  try {
    status.replaceWith(...await read());
  } catch (error) {
    const known = error instanceof PreviewError;
    const message = known ? error.message : url ? 'linkPreviewUnavailable' : 'linkPreviewFailed';
    status.replaceChildren(element('p', '', t(message)));
    const actions = element('div', 'link-preview-actions');
    if (url) {
      const original = element('a', 'secondary link-load-button', t('openInBrowser'));
      original.href = url.href;
      original.target = '_blank';
      original.rel = 'noopener noreferrer';
      actions.append(original);
    }
    if (message === 'linkPreviewTemporary' || (!known && !url)) {
      actions.append(action(t('retryLinkPreview'), () => loadContent(status, read, t, url)));
    }
    if (actions.childElementCount) status.append(actions);
  }
}

function statusBlock() {
  const status = element('div', 'link-preview-status');
  status.setAttribute('role', 'status');
  return status;
}

function diffFile(file, t) {
  const details = element('details', 'link-diff-file');
  const summary = element('summary');
  const counts = element('span', 'link-diff-counts');
  counts.append(element('span', 'link-diff-add', '+' + file.additions), element('span', 'link-diff-remove', '−' + file.deletions));
  summary.append(element('span', 'link-diff-path', file.filename), counts);
  details.append(summary);
  details.addEventListener('toggle', () => {
    if (!details.open || details.childElementCount > 1) return;
    if (typeof file.patch !== 'string') {
      details.append(element('p', 'link-preview-status', t('githubPatchUnavailable')));
      return;
    }
    const patch = element('pre', 'link-diff-patch');
    const code = element('code');
    for (const line of file.patch.split('\n')) {
      const style = { '+': 'link-diff-add', '-': 'link-diff-remove', '@': 'link-diff-hunk' }[line[0]];
      code.append(element('span', style, line + '\n'));
    }
    patch.append(code);
    details.append(patch);
  });
  return details;
}

function renderDiff(page, source, url, t) {
  const status = statusBlock();
  page.append(element('h2', 'link-diff-heading', source.label), status);
  async function read(number) {
    const endpoint = new URL(source.endpoint);
    endpoint.search = new URLSearchParams({ per_page: filesPerPage, page: number });
    const response = await publicContent(endpoint, 'application/vnd.github+json', 'application/json');
    const files = await response.json();
    const entries = files.map((file) => diffFile(file, t));
    if (!entries.length && number === 1) entries.push(element('p', 'link-preview-status', t('githubNoChanges')));
    if (number < maxDiffPages && /;\s*rel="next"/.test(response.headers.get('link') || '')) {
      const more = statusBlock();
      const loadMore = action(t('githubLoadFiles'), () => loadContent(more, () => read(number + 1), t, url));
      more.append(loadMore);
      entries.push(more);
    }
    return entries;
  }
  loadContent(status, () => read(1), t, url);
}

function linkFrame(label, sandbox) {
  const frame = element('iframe', 'link-frame');
  frame.title = label;
  frame.referrerPolicy = 'no-referrer';
  frame.setAttribute('sandbox', sandbox);
  return frame;
}

export function syncFrameAppearance(frameDocument) {
  const root = frameDocument?.documentElement;
  if (!root?.classList.contains('workbench-link-frame')) return;
  root.dataset.theme = document.documentElement.dataset.theme;
  root.dataset.motion = document.documentElement.dataset.motion;
}

function renderContentFrame(page, source, { label, onEscape }, render) {
  // The parent builds the preview and owns its interactions. The frame runs no scripts.
  const frame = linkFrame(label, 'allow-same-origin allow-popups allow-popups-to-escape-sandbox');
  frame.srcdoc = '<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">'
    + '<main class="link-document scrollable" tabindex="0"></main>';
  frame.addEventListener('load', () => {
    const frameDocument = frame.contentDocument;
    const content = frameDocument?.querySelector('main');
    if (!content) return;
    frameDocument.documentElement.classList.add('workbench-link-frame');
    frameDocument.documentElement.lang = document.documentElement.lang;
    syncFrameAppearance(frameDocument);
    const policy = frameDocument.createElement('meta');
    policy.httpEquiv = 'Content-Security-Policy';
    policy.content = "default-src 'none'; style-src 'self' 'unsafe-inline'; base-uri 'none'; form-action 'none'; img-src data:"
      + (source.imageRoot ? ' ' + source.imageRoot : '');
    const style = frameDocument.createElement('link');
    style.rel = 'stylesheet';
    style.href = new URL('./style.css?v=WORKBENCH_CACHE_VERSION', import.meta.url).href;
    frameDocument.head.append(policy, style);
    frameDocument.addEventListener('keydown', (event) => {
      if (event.key === 'Escape') onEscape(event);
    });
    render(content);
  }, { once: true });
  page.append(frame);
}

function articleSections(article) {
  const sections = new Map();
  for (const heading of article.querySelectorAll('h1, h2, h3, h4, h5, h6')) {
    const slug = heading.textContent.trim().toLowerCase().replace(/[^\p{L}\p{M}\p{N}_ -]/gu, '').replace(/ /g, '-');
    let name = slug;
    for (let count = 1; sections.has(name); count++) name = slug + '-' + count;
    sections.set(name, heading);
  }
  return (hash) => {
    if (!hash) return false;
    let name;
    try { name = decodeURIComponent(hash.slice(1)); } catch { return false; }
    const heading = sections.get(name);
    heading?.scrollIntoView({ block: 'start' });
    return Boolean(heading);
  };
}

function mdnMarkdown(raw) {
  const frontMatter = /^---\r?\n([\s\S]*?)\r?\n---\r?\n/.exec(raw);
  const title = /^title:[ \t]*([^\r\n]+)/m.exec(frontMatter?.[1] || '')?.[1].trim().replace(/^(['"])(.*)\1$/, '$2');
  const body = raw.slice(frontMatter?.[0].length || 0).replace(/{{\s*[\w]+(?:\(([^{}\r\n]*)\))?\s*}}/g, (_macro, args = '') => {
    const labels = [...args.matchAll(/"([^"]*)"/g)].map((match) => match[1]);
    return labels[1] || labels[0] || '';
  });
  return title ? '# ' + title + '\n\n' + body : body;
}

function prepareImages(fragment, source, origin) {
  for (const image of fragment.querySelectorAll('img[src]')) {
    let url;
    const base = source.raw || (source.mdn ? source.endpoint : origin);
    try { url = new URL(image.getAttribute('src'), base); } catch { /* Fall back to the image description. */ }
    if (source.android && url?.origin === androidOrigin) url.hostname = 'developer.android.google.cn';
    if (!source.imageRoot || !url || url.username || url.password || !url.href.startsWith(source.imageRoot)) {
      image.replaceWith(document.createTextNode(image.alt));
      continue;
    }
    image.src = url.href;
    image.referrerPolicy = 'no-referrer';
    image.loading = 'lazy';
    image.decoding = 'async';
  }
}

let readmeMarkdown;

function renderReadme(raw, base) {
  if (!readmeMarkdown) {
    // Parse GitHub's inline HTML syntax, but omit raw HTML from the preview.
    readmeMarkdown = window.markdownit({ html: true, linkify: true });
    readmeMarkdown.renderer.rules.html_block = () => '';
    readmeMarkdown.renderer.rules.html_inline = () => '';
  }
  return { html: readmeMarkdown.render(raw), base };
}

async function readGitHubReadme(source, url) {
  const repository = url.pathname.replace(/\/$/, '');
  const path = repository + '/blob/HEAD/README.md';
  const base = new URL(path, githubOrigin);
  try {
    const response = await publicContent(source.endpoint, 'application/vnd.github+json', 'application/json');
    const { content, encoding, size, name, html_url: htmlURL } = await response.json();
    if (encoding !== 'base64' || !content || size > 512 * 1024) throw new PreviewError('linkPreviewUnavailable');
    const raw = new TextDecoder().decode(Uint8Array.from(atob(content), (char) => char.charCodeAt(0)));
    const documentURL = typeof htmlURL === 'string' && htmlURL.toLowerCase().startsWith((githubOrigin + repository + '/blob/').toLowerCase())
      ? new URL(htmlURL) : base;
    if (!/\.(?:md|markdown|mdown|mkdn|mkd|mdwn)$/i.test(name)) return { plain: raw, base: documentURL };
    return renderReadme(raw, documentURL);
  } catch (error) {
    // Public root READMEs can also be fetched without the REST API's anonymous quota.
    try {
      const response = await publicContent(githubMarkdownEndpoint(path), 'text/plain');
      return renderReadme(await response.text(), base);
    } catch { throw error; }
  }
}

async function readArticle(source, url, markdown) {
  if (source.kind === 'readme') return readGitHubReadme(source, url);
  const response = await publicContent(source.endpoint, source.mime);
  const raw = await response.text();
  return { html: markdown.render(source.mdn ? mdnMarkdown(raw) : raw), base: url };
}

function bindArticleLinks(article, url, base, openLink) {
  const jump = articleSections(article);
  for (const link of article.querySelectorAll('a[href]')) {
    const href = link.getAttribute('href');
    let target;
    try { target = new URL(href, href.startsWith('#') ? url : base); } catch { continue; }
    link.href = target.href;
    link.target = '_blank';
    link.rel = 'noopener noreferrer';
  }
  article.addEventListener('click', (event) => {
    if (event.button || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
    const link = event.target.closest('a[href]');
    if (!link) return;
    if ((link.origin === url.origin && link.pathname === url.pathname && jump(link.hash)) || openLink(link)) {
      event.preventDefault();
    }
  });
  requestAnimationFrame(() => jump(url.hash));
}

function renderArticle(page, source, url, { t, markdown, openLink }) {
  const status = statusBlock();
  page.append(status);
  loadContent(status, async () => {
    if (source.kind === 'reader') {
      const response = await publicContent(source.endpoint, 'text/plain');
      const article = readerArticle(await response.text(), url);
      if (!article) throw new PreviewError('linkPreviewUnavailable');
      bindArticleLinks(article, url, url, openLink);
      return [article];
    }
    const { html, plain, base } = await readArticle(source, url, markdown);
    const article = element('article', 'message-content');
    if (plain !== undefined) {
      const pre = element('pre');
      pre.append(element('code', '', plain));
      article.append(pre);
      return [article];
    }
    const template = element('template');
    template.innerHTML = html;
    prepareImages(template.content, source, base);
    renderMarkdownDiagrams(template.content, t);
    article.append(template.content);
    bindArticleLinks(article, url, base, openLink);
    return [article];
  }, t, url);
}

export function createLinkPage(url, source, options) {
  const local = source.kind === 'file' || source.kind === 'diagram';
  const page = element('section', local ? 'link-page link-document scrollable' : 'link-page link-framed');
  if (source.kind === 'diagram') {
    page.append(diagramFigure(source.text, options.t));
    return page;
  }
  if (source.kind === 'iframe') {
    const frame = linkFrame(options.label, 'allow-scripts allow-forms');
    frame.src = url.href;
    page.append(frame);
    return page;
  }
  if (source.kind === 'file') {
    const status = statusBlock();
    page.append(status);
    loadContent(status, async () => {
      try {
        const file = await options.readFile(source.target, source.path);
        return [renderWorkspaceFile(file, source, options)];
      } catch (error) {
        const key = error.message === 'Unknown node operation.' ? 'filePreviewUpdateNode'
          : { 403: 'filePreviewOutside', 404: 'filePreviewMissing', 413: 'filePreviewTooLarge', 415: 'filePreviewUnsupported' }[error.status];
        if (key) throw new PreviewError(key);
        throw error;
      }
    }, options.t);
    return page;
  }
  renderContentFrame(page, source, options, (content) => {
    if (source.kind === 'diff') renderDiff(content, source, url, options.t);
    else renderArticle(content, source, url, options);
  });
  return page;
}
