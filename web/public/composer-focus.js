export function bindComposerFocus(input, { canEdit, isComposing, dialogs }) {
  let needsRefocus = false;

  function invalidate() { needsRefocus = true; }

  window.addEventListener('blur', () => { if (document.activeElement === input) invalidate(); });
  window.addEventListener('pageshow', (event) => { if (event.persisted) invalidate(); });
  document.addEventListener('visibilitychange', () => {
    if (document.hidden && document.activeElement === input) invalidate();
  });
  for (const dialog of dialogs) dialog.addEventListener('close', invalidate);

  input.addEventListener('click', (event) => {
    if (!event.isTrusted || !canEdit() || isComposing()) return;
    if (document.activeElement !== input) {
      needsRefocus = false;
      input.focus({ preventScroll: true });
      return;
    }
    if (!needsRefocus || input.selectionStart !== input.selectionEnd) return;
    // Android can keep DOM focus after its keyboard connection has been lost.
    // Reconnect on the next deliberate tap, within the browser's user gesture.
    needsRefocus = false;
    const caret = input.selectionStart;
    input.blur();
    input.focus({ preventScroll: true });
    input.setSelectionRange(caret, caret);
  });

  return invalidate;
}
