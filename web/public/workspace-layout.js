const chatMinimum = 430;
const separatorSize = 1;
const limits = {
  list: { min: 304, max: 480, fraction: 0.25 },
  preview: { min: 320, max: 760, fraction: 0.36 },
};
const twoColumnsAt = Math.max(768, chatMinimum + Math.max(limits.list.min, limits.preview.min) + separatorSize);
const threeColumnsAt = chatMinimum + limits.list.min + limits.preview.min + 2 * separatorSize;
export const singleColumnQuery = '(width < ' + twoColumnsAt + 'px)';

const storageKey = 'workbench.panel-widths';
const clamp = (value, min, max) => Math.max(min, Math.min(value, max));

export function mountWorkspaceLayout(workspace) {
  const preview = workspace.querySelector('#link-panel');
  const separators = [...workspace.querySelectorAll('[data-resize-panel]')];
  let preference = {};
  try { preference = JSON.parse(localStorage.getItem(storageKey)) || {}; } catch {}
  let width = 0;
  let sizes = {};
  let budget = 0;
  let drag;

  function paintSizes() {
    for (const separator of separators) {
      const name = separator.dataset.resizePanel;
      if (!Object.hasOwn(sizes, name)) continue;
      const range = rangeFor(name);
      workspace.style.setProperty('--' + name + '-width', sizes[name] + 'px');
      separator.setAttribute('aria-valuemin', Math.round(range.min));
      separator.setAttribute('aria-valuemax', Math.round(range.max));
      separator.setAttribute('aria-valuenow', Math.round(sizes[name]));
    }
  }

  function refresh() {
    const measuredWidth = workspace.getBoundingClientRect().width;
    if (!measuredWidth) { endDrag(); return; }
    let layout = 'single';
    let sides = [];
    if (measuredWidth >= twoColumnsAt) {
      sides = preview.hidden ? ['list'] : measuredWidth < threeColumnsAt ? ['preview'] : ['list', 'preview'];
      layout = sides.length === 2 ? 'three' : sides[0];
    }
    if (measuredWidth === width && layout === workspace.dataset.layout) return;
    endDrag();
    width = measuredWidth;
    workspace.dataset.layout = layout;
    budget = width - chatMinimum - sides.length * separatorSize;
    const minimum = sides.reduce((sum, name) => sum + limits[name].min, 0);
    const preferred = sides.map((name) => {
      const { min, max, fraction } = limits[name];
      const remembered = preference[name];
      return clamp(Number.isFinite(remembered) ? remembered : width * fraction, min, max);
    });
    const extra = preferred.reduce((sum, value, index) => sum + value - limits[sides[index]].min, 0);
    const share = extra ? Math.min(1, (budget - minimum) / extra) : 1;
    sizes = Object.fromEntries(sides.map((name, index) => {
      const { min } = limits[name];
      return [name, min + (preferred[index] - min) * share];
    }));
    paintSizes();
  }

  function rangeFor(name) {
    const occupied = Object.entries(sizes).reduce((sum, [side, value]) => side === name ? sum : sum + value, 0);
    const { min, max } = limits[name];
    return { min, max: clamp(budget - occupied, min, max) };
  }

  function resize(name, value) {
    const { min, max } = rangeFor(name);
    sizes[name] = clamp(value, min, max);
    paintSizes();
  }

  function remember() {
    preference = { ...preference, ...sizes };
    try { localStorage.setItem(storageKey, JSON.stringify(preference)); } catch {}
  }

  function endDrag() {
    if (!drag) return;
    const { separator, pointer, moved } = drag;
    drag = null;
    delete workspace.dataset.resizing;
    if (separator.hasPointerCapture(pointer)) separator.releasePointerCapture(pointer);
    if (moved) remember();
  }

  for (const separator of separators) {
    const name = separator.dataset.resizePanel;
    const direction = name === 'list' ? 1 : -1;
    separator.addEventListener('pointerdown', (event) => {
      if (drag || event.button !== 0 || !Object.hasOwn(sizes, name)) return;
      event.preventDefault();
      separator.focus({ preventScroll: true });
      separator.setPointerCapture(event.pointerId);
      drag = { separator, pointer: event.pointerId, x: event.clientX, width: sizes[name], moved: false };
      workspace.dataset.resizing = name;
    });
    separator.addEventListener('pointermove', (event) => {
      if (drag?.pointer !== event.pointerId || !Object.hasOwn(sizes, name)) return;
      const delta = direction * (event.clientX - drag.x);
      if (!delta && !drag.moved) return;
      drag.moved = true;
      resize(name, drag.width + delta);
    });
    for (const event of ['pointerup', 'pointercancel', 'lostpointercapture']) {
      separator.addEventListener(event, ({ pointerId }) => { if (drag?.pointer === pointerId) endDrag(); });
    }
    separator.addEventListener('keydown', (event) => {
      if (!Object.hasOwn(sizes, name) || !['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
      event.preventDefault();
      const { min, max } = rangeFor(name);
      const step = direction * (event.shiftKey ? 48 : 12);
      const next = { Home: min, End: max, ArrowLeft: sizes[name] - step, ArrowRight: sizes[name] + step };
      resize(name, next[event.key]);
      remember();
    });
  }
  new ResizeObserver(refresh).observe(workspace);
  return { refresh };
}
