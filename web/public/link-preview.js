const githubOrigin = 'https://github.com';
const docsOrigin = 'https://docs.github.com';
const mdnOrigin = 'https://developer.mozilla.org';
const rawOrigin = 'https://raw.githubusercontent.com';
const androidOrigin = 'https://developer.android.com';
const androidMirror = 'https://developer.android.google.cn';
const githubImages = docsOrigin + '/assets/images/';
const filesPerPage = 30;
const maxDiffPages = 3000 / filesPerPage;

export function linkPreviewSource(url) {
  if (!['github.com', 'docs.github.com', 'developer.mozilla.org', 'developer.android.com', 'developer.android.google.cn'].includes(url.hostname)) return undefined;
  if (url.protocol !== 'https:' || url.port || url.username || url.password) return null;
  if (url.origin === githubOrigin) {
    const match = /^\/([a-z\d-]{1,39})\/([a-z\d_.-]{1,100})(?:\/pull\/([1-9]\d{0,14})\/files)?\/?$/i.exec(url.pathname);
    if (!match) return null;
    const [, owner, repo, pull] = match;
    const endpoint = 'https://api.github.com/repos/' + owner + '/' + repo;
    if (!pull) return { kind: 'readme', endpoint: endpoint + '/readme' };
    return { kind: 'diff', label: owner + '/' + repo + ' #' + pull,
      endpoint: endpoint + '/pulls/' + pull + '/files' };
  }
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
    mime: 'text/plain', imageRoot };
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

class PreviewError extends Error {
  constructor(message, retryable = false) {
    super(message);
    this.retryable = retryable;
  }
}

async function publicContent(endpoint, accept, mime = accept) {
  const response = await fetch(endpoint, {
    credentials: typeof endpoint === 'string' && endpoint.startsWith('/api/') ? 'same-origin' : 'omit',
    redirect: 'error',
    referrerPolicy: 'no-referrer',
    headers: { accept },
    signal: AbortSignal.timeout(10_000),
  });
  if (response.ok && response.headers.get('content-type')?.split(';', 1)[0] === mime) {
    return response;
  }
  if (response.status === 404) throw new PreviewError('linkPreviewUnavailable');
  if (response.status === 429 || response.headers.get('x-ratelimit-remaining') === '0') {
    throw new PreviewError('linkPreviewRateLimit', true);
  }
  throw new PreviewError('linkPreviewFailed', true);
}

async function loadContent(status, read, t) {
  status.textContent = t('loadingLinkPreview');
  try {
    status.replaceWith(...await read());
  } catch (error) {
    const known = error instanceof PreviewError;
    status.replaceChildren(element('p', '', t(known ? error.message : 'linkPreviewFailed')));
    if (!known || error.retryable) status.append(action(t('retryLinkPreview'), () => loadContent(status, read, t)));
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

function renderDiff(page, source, t) {
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
      const loadMore = action(t('githubLoadFiles'), () => loadContent(more, () => read(number + 1), t));
      more.append(loadMore);
      entries.push(more);
    }
    return entries;
  }
  loadContent(status, () => read(1), t);
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
    const base = source.mime === 'text/plain' ? source.endpoint : origin;
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

async function readArticle(source, url, markdown) {
  if (source.kind !== 'readme') {
    const response = await publicContent(source.endpoint, source.mime);
    const raw = await response.text();
    return { html: markdown.render(source.mime === 'text/plain' ? mdnMarkdown(raw) : raw), base: url };
  }

  const response = await publicContent(source.endpoint, 'application/vnd.github+json', 'application/json');
  const { content, encoding, size, name, html_url: htmlURL } = await response.json();
  if (encoding !== 'base64' || !content || size > 512 * 1024) throw new PreviewError('linkPreviewUnavailable');
  const raw = new TextDecoder().decode(Uint8Array.from(atob(content), (char) => char.charCodeAt(0)));
  const repository = url.pathname.replace(/\/$/, '');
  let base = new URL(repository + '/blob/HEAD/README', githubOrigin);
  if (typeof htmlURL === 'string' && htmlURL.toLowerCase().startsWith((githubOrigin + repository + '/blob/').toLowerCase())) {
    base = new URL(htmlURL);
  }
  if (!/\.(?:md|markdown|mdown|mkdn|mkd|mdwn)$/i.test(name)) return { plain: raw, base };
  if (!readmeMarkdown) {
    // Parse GitHub's inline HTML syntax, but omit raw HTML from the preview.
    readmeMarkdown = window.markdownit({ html: true, linkify: true });
    readmeMarkdown.renderer.rules.html_block = () => '';
    readmeMarkdown.renderer.rules.html_inline = () => '';
  }
  return { html: readmeMarkdown.render(raw), base };
}

function renderArticle(page, source, url, { t, markdown, openLink }) {
  const status = statusBlock();
  page.append(status);
  loadContent(status, async () => {
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
    article.append(template.content);
    const jump = articleSections(article);
    for (const link of article.querySelectorAll('a[href]')) {
      const href = link.getAttribute('href');
      let target;
      try { target = new URL(href, href.startsWith('#') ? url : base); } catch { continue; }
      if (target.protocol !== 'https:' && target.protocol !== 'http:') continue;
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
    return [article];
  }, t);
}

export function createLinkPage(url, label, options) {
  const source = linkPreviewSource(url);
  const page = element('section', source === undefined ? 'link-page' : 'link-page link-document');
  if (source === undefined) {
    const frame = element('iframe', 'link-frame');
    frame.title = label;
    frame.setAttribute('sandbox', 'allow-scripts allow-forms');
    frame.referrerPolicy = 'no-referrer';
    frame.src = url.href;
    page.append(frame);
  } else if (!source) {
    page.append(element('p', 'link-preview-status', options.t('linkPreviewUnavailable')));
  } else if (source.kind === 'diff') {
    renderDiff(page, source, options.t);
  } else {
    renderArticle(page, source, url, options);
  }
  return page;
}
