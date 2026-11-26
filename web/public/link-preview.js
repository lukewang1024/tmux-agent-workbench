const githubOrigin = 'https://github.com';
const docsOrigin = 'https://docs.github.com';
const filesPerPage = 30;
const maxDiffPages = 3000 / filesPerPage;

export function githubPreviewSource(url) {
  if (url.hostname !== 'github.com' && url.hostname !== 'docs.github.com') return undefined;
  if (url.username || url.password) return null;
  if (url.origin === githubOrigin) {
    const match = /^\/([a-z\d-]{1,39})\/([a-z\d_.-]{1,100})\/pull\/([1-9]\d{0,14})\/files\/?$/i.exec(url.pathname);
    if (!match) return null;
    const [, owner, repo, pull] = match;
    return { kind: 'diff', label: owner + '/' + repo + ' #' + pull,
      endpoint: 'https://api.github.com/repos/' + owner + '/' + repo + '/pulls/' + pull + '/files' };
  }
  if (url.origin !== docsOrigin || !/^\/[a-z]{2}(?:-[a-z]{2})?(?:\/[a-z\d][a-z\d.@_-]*){2,}\/?$/i.test(url.pathname)) return null;
  return { kind: 'article', endpoint: docsOrigin + '/api/article/body?' +
    new URLSearchParams({ pathname: url.pathname.replace(/\/$/, '') }) };
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

async function publicContent(endpoint, accept) {
  const response = await fetch(endpoint, {
    credentials: 'omit',
    redirect: 'error',
    referrerPolicy: 'no-referrer',
    headers: { accept },
    signal: AbortSignal.timeout(10_000),
  });
  if (response.ok && response.headers.get('content-type')?.includes(accept.includes('json') ? 'application/json' : 'text/markdown')) {
    return response;
  }
  if (response.status === 404) throw new PreviewError('githubPreviewUnavailable');
  if (response.status === 429 || response.headers.get('x-ratelimit-remaining') === '0') {
    throw new PreviewError('githubPreviewRateLimit', true);
  }
  throw new PreviewError('githubPreviewFailed', true);
}

async function loadContent(status, read, t) {
  status.textContent = t('loadingLinkPreview');
  try {
    status.replaceWith(...await read());
  } catch (error) {
    const known = error instanceof PreviewError;
    status.replaceChildren(element('p', '', t(known ? error.message : 'githubPreviewFailed')));
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
    const response = await publicContent(endpoint, 'application/vnd.github+json');
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

function renderArticle(page, source, url, { t, markdown, openLink }) {
  const status = statusBlock();
  page.append(status);
  loadContent(status, async () => {
    const response = await publicContent(source.endpoint, 'text/markdown');
    const article = element('article', 'message-content');
    article.innerHTML = markdown.render(await response.text());
    const jump = articleSections(article);
    for (const link of article.querySelectorAll('a[href]')) {
      let target;
      try { target = new URL(link.getAttribute('href'), url); } catch { continue; }
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
  const source = githubPreviewSource(url);
  const page = element('section', source === undefined ? 'link-page' : 'link-page link-document');
  if (source === undefined) {
    const frame = element('iframe', 'link-frame');
    frame.title = label;
    frame.setAttribute('sandbox', 'allow-scripts allow-forms');
    frame.referrerPolicy = 'no-referrer';
    frame.src = url.href;
    page.append(frame);
  } else if (!source) {
    page.append(element('p', 'link-preview-status', options.t('githubPreviewUnavailable')));
  } else if (source.kind === 'diff') {
    renderDiff(page, source, options.t);
  } else {
    renderArticle(page, source, url, options);
  }
  return page;
}
