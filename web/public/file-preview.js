import { make } from './dom.js?v=WORKBENCH_CACHE_VERSION';
import { workspaceLink } from './link-policy.js?v=WORKBENCH_CACHE_VERSION';

const imageTypes = new Set(['image/png', 'image/jpeg', 'image/gif', 'image/webp']);
const encodePath = (path) => path.split('/').map(encodeURIComponent).join('/');

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
  const pre = make('pre', 'workspace-file-text');
  const code = make('code');
  if (source.line) {
    const rows = text.split('\n');
    const index = source.line - 1;
    if (index < rows.length) {
      const line = make('mark', '', rows[index] || ' ');
      code.append(index ? rows.slice(0, index).join('\n') + '\n' : '', line, rows.slice(index + 1).map((row) => '\n' + row).join(''));
      requestAnimationFrame(() => line.scrollIntoView({ block: 'center' }));
    } else code.textContent = text;
  } else code.textContent = text;
  pre.append(code);
  return pre;
}
