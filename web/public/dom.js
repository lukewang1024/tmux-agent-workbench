export function make(tag, className, text) {
  const element = document.createElement(tag);
  if (className) element.className = className;
  if (text !== undefined) element.textContent = text;
  return element;
}

export function button(text, className, action) {
  const element = make('button', className, text);
  element.type = 'button';
  element.addEventListener('click', action);
  return element;
}

export function glyph(name, { inline = false } = {}) {
  const icon = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  icon.setAttribute('class', 'glyph');
  icon.setAttribute('aria-hidden', 'true');
  if (inline) {
    const symbol = document.getElementById('icon-' + name);
    icon.setAttribute('viewBox', symbol.getAttribute('viewBox'));
    icon.append(...Array.from(symbol.children, (child) => child.cloneNode(true)));
    return icon;
  }
  const use = document.createElementNS(icon.namespaceURI, 'use');
  use.setAttribute('href', '#icon-' + name);
  icon.append(use);
  return icon;
}

export function bindBackButton(element, onBack) {
  function setLabel(label) {
    element.setAttribute('aria-label', label);
    element.title = label;
  }
  element.classList.add('back-button');
  element.replaceChildren(glyph('back'));
  element.addEventListener('click', onBack);
  setLabel(element.getAttribute('aria-label'));
  return setLabel;
}

export function restoreFocusOnFrame(element) {
  requestAnimationFrame(() => {
    if (element.isConnected && document.activeElement === document.body) element.focus({ preventScroll: true });
  });
}
