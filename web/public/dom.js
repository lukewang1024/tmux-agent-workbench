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

export function glyph(name) {
  const icon = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  const use = document.createElementNS(icon.namespaceURI, 'use');
  icon.setAttribute('class', 'glyph');
  icon.setAttribute('aria-hidden', 'true');
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
