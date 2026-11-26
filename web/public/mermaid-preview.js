import { button, make } from './dom.js?v=WORKBENCH_CACHE_VERSION';

const maxSource = 24_000;
const maxInlineDiagrams = 16;
let renderer;
let queue = Promise.resolve();

function loadRenderer() {
  renderer ??= new Promise((resolve, reject) => {
    const frame = make('iframe', 'mermaid-renderer');
    frame.title = 'Mermaid';
    frame.tabIndex = -1;
    frame.setAttribute('aria-hidden', 'true');
    frame.setAttribute('sandbox', 'allow-scripts');
    frame.src = new URL('./mermaid-renderer.html?v=WORKBENCH_CACHE_VERSION', import.meta.url).href;
    const timeout = setTimeout(() => {
      frame.remove();
      reject(new Error('Diagram renderer did not load'));
    }, 15_000);
    frame.addEventListener('load', () => {
      clearTimeout(timeout);
      resolve(frame);
    }, { once: true });
    document.body.append(frame);
  }).catch((error) => {
    renderer = undefined;
    throw error;
  });
  return renderer;
}

async function requestDiagram(text) {
  if (!text.trim() || text.length > maxSource) throw new Error('Diagram size is unsupported');
  const frame = await loadRenderer();
  const theme = matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'default';
  return new Promise((resolve, reject) => {
    const { port1, port2 } = new MessageChannel();
    const timeout = setTimeout(() => {
      port1.close();
      frame.remove();
      renderer = undefined;
      reject(new Error('Diagram rendering timed out'));
    }, 10_000);
    port1.onmessage = ({ data }) => {
      clearTimeout(timeout);
      port1.close();
      if (typeof data?.svg === 'string' && data.svg.length <= 1_000_000) resolve(data);
      else reject(new Error('Could not render the diagram'));
    };
    // The script-enabled renderer has an opaque origin. Only this port receives its result.
    frame.contentWindow.postMessage({ text, theme }, '*', [port2]);
  });
}

export function diagramFigure(text, t) {
  const figure = make('figure', 'mermaid-figure');
  const status = make('p', 'link-preview-status', t('loadingDiagram'));
  status.setAttribute('role', 'status');
  const source = make('details', 'diagram-source');
  const pre = make('pre');
  pre.append(make('code', 'language-mermaid', text));
  source.append(make('summary', '', t('diagramSource')), pre);
  figure.append(status, source);

  const result = queue.then(() => requestDiagram(text));
  queue = result.catch(() => {});
  result.then(({ svg, width }) => {
    const image = make('img');
    image.alt = t('mermaidDiagram');
    if (Number.isFinite(width) && width > 0) image.width = Math.min(4096, Math.ceil(width));
    const imageView = make('div', 'diagram-image');
    image.src = 'data:image/svg+xml;charset=utf-8,' + encodeURIComponent(svg);
    imageView.append(image);
    status.replaceWith(imageView);
  }).catch(() => {
    status.textContent = t('diagramFailed');
    source.open = true;
  });
  return figure;
}

export function renderMarkdownDiagrams(container, t) {
  const diagrams = [...container.querySelectorAll('pre > code.language-mermaid')].slice(0, maxInlineDiagrams);
  for (const code of diagrams) code.parentElement.replaceWith(diagramFigure(code.textContent, t));
}

export function addDiagramButtons(container, t, open) {
  for (const code of container.querySelectorAll('pre > code.language-mermaid')) {
    const text = code.textContent;
    const preview = button(t('previewDiagram'), 'secondary diagram-trigger', () => open(text, preview));
    code.parentElement.before(preview);
  }
}
