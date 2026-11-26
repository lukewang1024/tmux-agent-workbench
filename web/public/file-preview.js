import { make } from './dom.js?v=WORKBENCH_CACHE_VERSION';
import { workspaceLink } from './link-policy.js?v=WORKBENCH_CACHE_VERSION';

const imageTypes = new Set(['image/png', 'image/jpeg', 'image/gif', 'image/webp']);
const encodePath = (path) => path.split('/').map(encodeURIComponent).join('/');
const sourceAliases = { jsonl: 'json', bashrc: 'bash', zshrc: 'bash' };

function renderSourceFile(text, name, line) {
  const contents = text.replace(/\r\n?/g, '\n');
  const lineCount = contents.split('\n').length;
  const view = make('div', 'workspace-file-source');
  const gutter = make('pre', 'workspace-file-gutter', Array.from({ length: lineCount }, (_, index) => index + 1).join('\n'));
  gutter.setAttribute('aria-hidden', 'true');
  const pre = make('pre', 'workspace-file-text');
  const code = make('code');
  const extension = name.toLowerCase().split('.').at(-1);
  const language = Object.hasOwn(sourceAliases, extension) ? sourceAliases[extension] : extension;
  const highlighter = window.hljs;
  if (highlighter?.getLanguage(language)) {
    code.innerHTML = highlighter.highlight(contents, { language, ignoreIllegals: true }).value;
  } else code.textContent = contents;
  pre.append(code);
  view.append(gutter, pre);
  if (line && line <= lineCount) {
    const highlight = make('mark', 'workspace-file-line');
    highlight.setAttribute('aria-hidden', 'true');
    highlight.style.setProperty('--line-index', line - 1);
    view.prepend(highlight);
    requestAnimationFrame(() => highlight.scrollIntoView({ block: 'center', inline: 'nearest' }));
  }
  return view;
}

export function renderWorkspaceFile(file, source, { fileMarkdown, openLink }) {
  if (imageTypes.has(file.mime)) {
    const image = make('img', 'workspace-file-image');
    image.alt = file.name;
    image.src = 'data:' + file.mime + ';base64,' + file.content;
    return image;
  }
  const text = new TextDecoder().decode(Uint8Array.from(atob(file.content), (character) => character.charCodeAt(0)));
  if (file.mime === 'text/markdown' && !source.line) {
    const article = make('article', 'message-content');
    const template = make('template');
    template.innerHTML = fileMarkdown.render(text);
    const base = 'file://' + encodePath(file.path);
    for (const image of template.content.querySelectorAll('img')) image.replaceWith(image.alt);
    for (const link of template.content.querySelectorAll('a[href]')) {
      const href = link.getAttribute('href');
      const local = workspaceLink(href);
      try {
        const target = new URL(local ? encodePath(local.path) : href, base);
        if (local?.line) target.hash = 'L' + local.line;
        link.href = target.href;
      } catch { link.removeAttribute('href'); }
      link.target = '_blank';
      link.rel = 'noopener noreferrer';
    }
    article.append(template.content);
    article.addEventListener('click', (event) => {
      if (event.button || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
      const link = event.target.closest('a[href]');
      if (link && openLink(link, source.target)) event.preventDefault();
    });
    return article;
  }
  return renderSourceFile(text, file.name, source.line);
}
