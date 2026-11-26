import { bindBackButton, button, glyph, make } from './dom.js?v=WORKBENCH_CACHE_VERSION';

const inset = 16;
const maxZoom = 4;
const zoomStep = 1.4;

function iconButton(icon, label, action) {
  const control = button('', 'row-action', action);
  control.title = label;
  control.setAttribute('aria-label', label);
  control.append(glyph(icon));
  return control;
}

export function openDiagramViewer(image, { width, height, t, opener }) {
  const dialog = make('dialog', 'diagram-viewer');
  const label = t('mermaidDiagram');
  dialog.setAttribute('aria-label', label);
  const header = make('header', 'toolbar diagram-toolbar');
  const back = make('button');
  back.type = 'button';
  back.setAttribute('aria-label', t('closeDiagram'));
  bindBackButton(back, () => dialog.close());
  const name = make('h2', 'diagram-title', label);
  const controls = make('div', 'diagram-controls');
  controls.setAttribute('role', 'group');
  controls.setAttribute('aria-label', t('diagramZoom'));
  const zoomOut = iconButton('minus', t('diagramZoomOut'), () => zoom(zoomState.getScale() / zoomStep));
  const zoomIn = iconButton('plus', t('diagramZoomIn'), () => zoom(zoomState.getScale() * zoomStep));
  const actual = button('1:1', 'row-action diagram-actual', () => zoom(1));
  actual.title = t('diagramActualSize');
  actual.setAttribute('aria-label', actual.title);
  const fitButton = iconButton('fit-diagram', t('diagramFit'), fit);
  const scaleLabel = make('output', 'diagram-scale');
  const percent = new Intl.NumberFormat(document.documentElement.lang, { style: 'percent' });
  controls.append(zoomOut, scaleLabel, zoomIn, actual, fitButton);
  header.append(back, name, controls);

  const viewport = make('div', 'diagram-viewport');
  viewport.tabIndex = 0;
  viewport.setAttribute('aria-label', label);
  const plane = image.cloneNode();
  plane.className = 'diagram-plane';
  plane.draggable = false;
  viewport.append(plane);
  dialog.append(header, viewport);
  dialog.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') event.stopPropagation();
  });
  document.body.append(dialog);
  dialog.showModal();

  let fitScale;
  const zoomState = window.Panzoom(plane, {
    canvas: true, minScale: 0.001, maxScale: maxZoom, panOnlyWhenZoomed: true, cursor: 'grab',
  });

  function paintControls() {
    const scale = zoomState.getScale();
    zoomOut.disabled = scale <= fitScale;
    zoomIn.disabled = scale >= maxZoom;
    scaleLabel.value = percent.format(scale);
  }

  function keepInView() {
    const scale = zoomState.getScale();
    const pan = zoomState.getPan();
    const bound = (value, size, available) => {
      const limit = Math.max(0, (size * scale + inset * 2 - available) / 2) / scale;
      return Math.max(-limit, Math.min(limit, value));
    };
    zoomState.pan(bound(pan.x, width, viewport.clientWidth), bound(pan.y, height, viewport.clientHeight), { force: true });
  }

  function fit() {
    zoomState.zoom(fitScale);
    zoomState.pan(0, 0, { force: true });
  }

  function resize() {
    const { clientWidth, clientHeight } = viewport;
    if (!clientWidth || !clientHeight) return;
    const fitted = fitScale === undefined || zoomState.getScale() === fitScale;
    fitScale = Math.min(1, Math.max(1, clientWidth - inset * 2) / width, Math.max(1, clientHeight - inset * 2) / height);
    plane.style.marginLeft = (clientWidth - width) / 2 + 'px';
    plane.style.marginTop = (clientHeight - height) / 2 + 'px';
    zoomState.setOptions({ minScale: fitScale, startScale: fitScale });
    if (fitted || zoomState.getScale() <= fitScale) fit();
    else keepInView();
    paintControls();
  }

  function zoom(scale) {
    const rect = viewport.getBoundingClientRect();
    zoomState.zoomToPoint(Math.max(fitScale, Math.min(maxZoom, scale)), {
      clientX: rect.left + rect.width / 2, clientY: rect.top + rect.height / 2,
    });
    keepInView();
  }

  plane.addEventListener('panzoomchange', paintControls);
  plane.addEventListener('panzoomend', keepInView);
  viewport.addEventListener('wheel', zoomState.zoomWithWheel, { passive: false });
  viewport.addEventListener('keydown', (event) => {
    const step = 44 / zoomState.getScale();
    const offset = { ArrowLeft: [step, 0], ArrowRight: [-step, 0], ArrowUp: [0, step], ArrowDown: [0, -step] }[event.key];
    if (!offset) return;
    event.preventDefault();
    zoomState.pan(...offset, { relative: true });
    keepInView();
  });
  const observer = new ResizeObserver(resize);
  observer.observe(viewport);
  resize();

  dialog.addEventListener('close', () => {
    observer.disconnect();
    zoomState.destroy();
    dialog.remove();
    if (opener.isConnected) opener.focus({ preventScroll: true });
  }, { once: true });
}
