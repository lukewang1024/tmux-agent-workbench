const pointer = matchMedia('(any-hover: hover) and (any-pointer: fine)');
const margin = 8;

export function actionMenuAnchor(event, element) {
  if (!event || event.pointerType === 'touch' || (!event.pointerType && !pointer.matches)) return null;
  if (event.type === 'contextmenu' && (event.clientX || event.clientY)) return { x: event.clientX, y: event.clientY };
  return element;
}

export function mountActionMenu(dialog) {
  const panel = dialog.querySelector('.sheet');
  const choices = () => [...panel.querySelectorAll('.sheet-row:not(:disabled)')];
  let anchor;
  let invoker;

  function place() {
    const atButton = anchor instanceof Element;
    if (atButton && !anchor.isConnected) return;
    const { left, right, top, bottom } = atButton ? anchor.getBoundingClientRect() : {
      left: anchor.x, right: anchor.x, top: anchor.y, bottom: anchor.y,
    };
    const gap = atButton ? 4 : 0;
    panel.style.removeProperty('--menu-max-height');
    const { width, height } = panel.getBoundingClientRect();
    const below = innerHeight - bottom - gap - margin;
    const above = top - gap - margin;
    const flip = height > below && above > below;
    const x = atButton ? right - width : left + width <= innerWidth - margin ? left : left - width;
    const y = flip ? Math.max(margin, top - gap - height) : Math.max(margin, bottom + gap);
    panel.style.setProperty('--menu-x', Math.max(margin, Math.min(x, innerWidth - width - margin)) + 'px');
    panel.style.setProperty('--menu-y', y + 'px');
    panel.style.setProperty('--menu-max-height', Math.max(0, flip ? above : below) + 'px');
  }

  function reset() {
    invoker?.setAttribute('aria-expanded', 'false');
    anchor = undefined;
    invoker = undefined;
  }

  dialog.addEventListener('close', reset);
  window.addEventListener('resize', () => { if (dialog.open && anchor) place(); });
  dialog.addEventListener('keydown', (event) => {
    if (!anchor || !['ArrowUp', 'ArrowDown', 'Home', 'End'].includes(event.key)) return;
    const items = choices();
    if (!items.length) return;
    const current = items.indexOf(document.activeElement);
    const next = { Home: 0, End: items.length - 1, ArrowUp: Math.max(0, current - 1), ArrowDown: Math.min(items.length - 1, current + 1) };
    event.preventDefault();
    items[next[event.key]].focus();
  });
  return {
    reset,
    open(target, trigger) {
      invoker = trigger;
      invoker?.setAttribute('aria-expanded', 'true');
      if (!target) return;
      anchor = target;
      dialog.dataset.position = 'anchored';
      place();
      (choices()[0] || panel).focus({ preventScroll: true });
    },
  };
}
