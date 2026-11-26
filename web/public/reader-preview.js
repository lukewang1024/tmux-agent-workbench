import { make } from './dom.js?v=WORKBENCH_CACHE_VERSION';

const allowed = new Set(['a', 'article', 'b', 'blockquote', 'br', 'caption', 'code', 'dd', 'del', 'div', 'dl', 'dt',
  'em', 'figcaption', 'figure', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'hr', 'i', 'kbd', 'li', 'ol', 'p', 'pre',
  's', 'samp', 'section', 'small', 'span', 'strong', 'sub', 'sup', 'table', 'tbody', 'td', 'th', 'thead', 'tr', 'ul']);
const omitted = new Set(['audio', 'base', 'embed', 'form', 'iframe', 'link', 'meta', 'noscript', 'object', 'script',
  'source', 'style', 'svg', 'template', 'video']);

function readerLink(href, url) {
  try {
    const target = new URL(href, url);
    if (['https:', 'http:'].includes(target.protocol) && !target.username && !target.password) return target.href;
  } catch { /* The label remains readable. */ }
  return null;
}

function appendReaderContent(parent, children, url) {
  for (const node of children) {
    if (node.nodeType === Node.TEXT_NODE) { parent.append(node.textContent); continue; }
    if (node.nodeType !== Node.ELEMENT_NODE || omitted.has(node.localName)) continue;
    if (node.localName === 'img') { parent.append(node.getAttribute('alt') || ''); continue; }
    if (!allowed.has(node.localName)) { appendReaderContent(parent, node.childNodes, url); continue; }
    const clean = make(node.localName);
    if (clean.localName === 'a' && node.hasAttribute('href')) {
      const target = readerLink(node.getAttribute('href'), url);
      if (target) {
        clean.href = target;
        clean.target = '_blank';
        clean.rel = 'noopener noreferrer';
      }
    }
    appendReaderContent(clean, node.childNodes, url);
    parent.append(clean);
  }
}

const readerStyle = 'html{color-scheme:light dark}body{margin:0;padding:14px;font:15px/1.6 system-ui;overflow-wrap:anywhere}'
  + 'h1,h2,h3,h4{line-height:1.3}pre{padding:8px;overflow:auto;background:color-mix(in srgb,CanvasText 7%,Canvas)}'
  + 'img{max-width:100%}table{border-collapse:collapse}td,th{padding:3px 8px;border:1px solid GrayText}';

export function linkFrame(title, permissions) {
  const frame = make('iframe', 'link-frame');
  frame.title = title;
  frame.referrerPolicy = 'no-referrer';
  frame.setAttribute('sandbox', permissions);
  return frame;
}

export function readerFrame(html, url, label) {
  const doc = document.implementation.createHTMLDocument();
  const input = doc.createElement('template');
  input.innerHTML = html;
  doc.title = input.content.querySelector('title')?.textContent || '';
  for (const node of input.content.querySelectorAll([...omitted, 'title'].join(','))) node.remove();
  for (const image of input.content.querySelectorAll('img')) image.replaceWith(image.getAttribute('alt') || '');
  for (const link of input.content.querySelectorAll('a[href]')) {
    const target = readerLink(link.getAttribute('href'), url);
    if (target) link.setAttribute('href', target);
    else link.removeAttribute('href');
  }
  doc.body.append(input.content);
  const parsed = new window.Readability(doc, { maxElemsToParse: 15000, disableJSONLD: true }).parse();
  if (!parsed?.content || !parsed.textContent?.trim()) return null;
  const frame = linkFrame(label, 'allow-popups allow-popups-to-escape-sandbox');
  const extracted = make('template');
  extracted.innerHTML = parsed.content;
  const fragment = make('article');
  if (parsed.title && !extracted.content.querySelector('h1')) fragment.append(make('h1', '', parsed.title));
  appendReaderContent(fragment, extracted.content.childNodes, url);
  frame.srcdoc = '<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">'
    + '<meta http-equiv="Content-Security-Policy" content="default-src \'none\'; style-src \'unsafe-inline\'; base-uri \'none\'; form-action \'none\'">'
    + '<style>' + readerStyle + '</style>' + fragment.outerHTML;
  return frame;
}
