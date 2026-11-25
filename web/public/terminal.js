const terminalKeys = {
  Esc: '\x1b', Tab: '\t', '↑': '\x1b[A', '↓': '\x1b[B', '←': '\x1b[D', '→': '\x1b[C',
  Home: '\x1b[H', End: '\x1b[F', PageUp: '\x1b[5~', PageDown: '\x1b[6~',
};

function decode(data) { return Uint8Array.from(atob(data), (character) => character.charCodeAt(0)); }

function terminalTheme() {
  const colors = getComputedStyle(document.documentElement);
  const color = (role) => colors.getPropertyValue('--' + role).trim();
  const ansiColors = ['black', 'red', 'green', 'yellow', 'blue', 'magenta', 'cyan', 'white',
    'brightBlack', 'brightRed', 'brightGreen', 'brightYellow', 'brightBlue', 'brightMagenta', 'brightCyan', 'brightWhite'];
  return {
    background: color('terminal'), foreground: color('terminal-ink'),
    cursor: color('terminal-caret'), cursorAccent: color('terminal'),
    ...Object.fromEntries(ansiColors.map((name, index) => [name, color('terminal-ansi-' + index)])),
  };
}

export class TerminalView {
  #current = null;
  #screen;
  #status;
  #control;
  #form;
  #input;
  #keys;
  #options;

  constructor(root, options) {
    this.#options = options;
    this.#screen = root.querySelector('#term-screen');
    this.#status = root.querySelector('#term-state');
    this.#control = root.querySelector('#control');
    this.#form = root.querySelector('#term-form');
    this.#input = this.#form.querySelector('input');
    const keyBar = root.querySelector('.keys');
    this.#keys = Array.from(keyBar.querySelectorAll('[data-key]'));

    this.#control.addEventListener('click', () => this.#send({ type: this.#current?.writable ? 'release' : 'take' }));
    this.#form.addEventListener('submit', (event) => {
      event.preventDefault();
      if (this.#sendKeys(this.#input.value, true)) this.#input.value = '';
    });
    for (const key of this.#keys) key.addEventListener('click', () => this.#customKey(key.dataset.key));
    for (const container of [this.#form, keyBar]) container.addEventListener('pointerdown', (event) => {
      const retainsFocus = document.activeElement === this.#input
        || (container === keyBar && document.activeElement === this.#current?.xterm.textarea);
      if (retainsFocus && event.target.closest('button')) event.preventDefault();
    });
  }

  hasInputFocus() {
    return document.activeElement === this.#input || document.activeElement === this.#current?.xterm.textarea;
  }

  updateTheme() {
    if (this.#current) this.#current.xterm.options.theme = terminalTheme();
  }

  open(node, pane) {
    const key = node + '/' + pane.key;
    if (this.#current?.key === key) return;
    this.close();
    const xterm = new window.Terminal({
      cols: Math.max(1, pane.cols), rows: Math.max(1, pane.rows), fontSize: 13,
      cursorBlink: false, scrollback: 2000, theme: terminalTheme(), disableStdin: true, cursorInactiveStyle: 'outline',
    });
    const fit = new window.FitAddon.FitAddon();
    const current = { key, node, pane, xterm, fit, writable: false, ready: false, ctrl: false, alt: false };
    this.#current = current;
    xterm.loadAddon(fit);
    xterm.open(this.#screen);
    xterm.onData((data) => this.#sendKeys(data));
    xterm.attachCustomKeyEventHandler((event) => !(event.ctrlKey && event.shiftKey && /^c$/i.test(event.key)));
    current.observer = new ResizeObserver(() => this.#fit(current));
    current.observer.observe(this.#screen);
    this.resume();
  }

  close() {
    const current = this.#current;
    if (!current) return;
    this.#current = null;
    clearTimeout(current.retry);
    current.observer.disconnect();
    current.socket?.close();
    current.xterm.dispose();
    this.#input.value = '';
    this.#screen.replaceChildren();
  }

  resume() {
    const current = this.#current;
    if (!current || !this.#options.available()
        || [WebSocket.OPEN, WebSocket.CONNECTING].includes(current.socket?.readyState)) return;
    clearTimeout(current.retry);
    current.ready = current.writable = current.ctrl = current.alt = false;
    this.#paint('connecting');
    const url = new URL('/api/terminal', location.href);
    url.search = new URLSearchParams({ node: current.node, pane: current.pane.key });
    url.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const socket = new WebSocket(url);
    current.socket = socket;
    socket.addEventListener('message', (event) => {
      if (this.#current !== current || current.socket !== socket) return;
      const packet = JSON.parse(event.data);
      if (packet.error) { this.#options.reportError(packet.error); return; }
      if (packet.initial) {
        current.ready = true;
        current.resizable = packet.resizable;
        current.xterm.resize(Math.max(1, packet.cols), Math.max(1, packet.rows));
        current.xterm.reset();
        current.xterm.write(decode(packet.initial));
        this.#paint(current.writable ? 'inputEnabled' : 'watching');
      }
      if (packet.data) current.xterm.write(decode(packet.data));
      if (typeof packet.writable === 'boolean') {
        current.writable = packet.writable;
        current.xterm.options.disableStdin = !current.writable;
        this.#paint(current.writable ? 'inputEnabled' : 'watching');
        if (current.writable) {
          this.#fit(current);
          if (!this.#options.mobile.matches) current.xterm.focus();
        }
      }
    });
    socket.addEventListener('close', (event) => {
      if (this.#current !== current || current.socket !== socket) return;
      current.writable = current.ready = false;
      current.xterm.options.disableStdin = true;
      this.#paint('disconnected');
      if (event.code === 4001) this.#options.requirePasskey();
      else current.retry = setTimeout(() => this.resume(), 2000);
    });
  }

  #paint(message) {
    const current = this.#current;
    const writable = current.writable;
    const t = this.#options.translate;
    const dot = this.#status.ownerDocument.createElement('span');
    dot.className = current.ready ? 'dot' : 'dot off';
    this.#status.replaceChildren(dot, t(message));
    this.#control.textContent = t(writable ? 'releaseInput' : 'enableInput');
    this.#control.disabled = !current.ready;
    this.#control.setAttribute('aria-pressed', String(writable));
    this.#form.hidden = !writable;
    for (const key of this.#keys) {
      key.disabled = !writable;
      if (key.dataset.key === 'Ctrl' || key.dataset.key === 'Alt') {
        key.setAttribute('aria-pressed', String(writable && current[key.dataset.key.toLowerCase()]));
      }
    }
  }

  #fit(current) {
    if (this.#current !== current || !current.resizable || !current.writable || !this.#options.available()) return;
    const dimensions = current.fit.proposeDimensions();
    if (!dimensions || !Number.isFinite(dimensions.cols) || !Number.isFinite(dimensions.rows)) return;
    const cols = Math.max(24, Math.min(360, dimensions.cols));
    const rows = Math.max(6, Math.min(160, dimensions.rows));
    if (cols === current.xterm.cols && rows === current.xterm.rows) return;
    if (this.#send({ type: 'resize', cols, rows })) current.xterm.resize(cols, rows);
  }

  #send(packet) {
    const current = this.#current;
    if (!this.#options.available() || current?.socket?.readyState !== WebSocket.OPEN) return false;
    if (!current.writable && ['input', 'resize'].includes(packet.type)) return false;
    if (packet.type === 'input' && packet.data.length) {
      // Split paste into bounded chunks without bisecting Unicode code points.
      const characters = Array.from(packet.data);
      for (let offset = 0; offset < characters.length; offset += 2500) {
        current.socket.send(JSON.stringify({
          type: 'input', data: characters.slice(offset, offset + 2500).join(''),
          bracketed: packet.submit, submit: packet.submit && offset + 2500 >= characters.length,
        }));
      }
    } else if (packet.type !== 'input' || packet.submit) current.socket.send(JSON.stringify(packet));
    return true;
  }

  #sendKeys(data, submit = false) {
    const current = this.#current;
    if (!current?.writable) return false;
    const control = current.ctrl && /^[a-z]$/i.test(data);
    const alt = current.alt;
    if (control) data = String.fromCharCode(data.toUpperCase().charCodeAt(0) - 64);
    if (alt) data = '\x1b' + data;
    if (!this.#send({ type: 'input', data, submit: submit && !control && !alt })) return false;
    current.ctrl = current.alt = false;
    this.#paint('inputEnabled');
    return true;
  }

  #customKey(key) {
    const current = this.#current;
    if (!current?.writable || !this.#options.available()) return;
    if (key === 'Ctrl' || key === 'Alt') {
      const name = key.toLowerCase();
      current[name] = !current[name];
      this.#paint('inputEnabled');
    } else this.#sendKeys(terminalKeys[key]);
  }
}
