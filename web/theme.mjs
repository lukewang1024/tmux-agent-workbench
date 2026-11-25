import { readFile } from 'node:fs/promises';

const hexColor = /^#[0-9a-f]{6}$/i;
const tmuxRoles = ['background', 'foreground', 'selection', 'muted', 'raised', 'accent', 'warning', 'alert', 'info'];
const ansiNames = ['black', 'red', 'green', 'yellow', 'blue', 'magenta', 'cyan', 'white'];
export const sidebarPalettes = {
  light: {
    paper: '#fafafa', ink: '#383a42', muted: '#62666f', active: '#e5e5e6', raised: '#d0d2d8',
    'state-idle': '#50a14f', 'state-working': '#c18401', 'state-blocked': '#e45649', 'state-done': '#4078f2',
  },
  dark: {
    paper: '#282c34', ink: '#aab2bf', muted: '#8d95a3', active: '#3e4452', raised: '#4b5263',
    'state-idle': '#98c379', 'state-working': '#e5c07b', 'state-blocked': '#e06c75', 'state-done': '#61afef',
  },
};
const { light, dark } = sidebarPalettes;
const defaultPalettes = {
  light: {
    background: light.paper, foreground: light.ink, cursor: light.muted,
    ansi: [light.ink, light['state-blocked'], light['state-idle'], light['state-working'], light['state-done'], '#a626a4', '#0184bc', '#a0a1a7',
      '#696c77', '#ca1243', light['state-idle'], light['state-working'], light['state-done'], '#a626a4', '#0184bc', light.paper],
  },
  dark: {
    background: dark.paper, foreground: dark.ink, cursor: dark.ink,
    ansi: [dark.paper, dark['state-blocked'], dark['state-idle'], dark['state-working'], dark['state-done'], '#c678dd', '#56b6c2', dark.ink,
      '#5c6370', dark['state-blocked'], dark['state-idle'], dark['state-working'], dark['state-done'], '#c678dd', '#56b6c2', '#ffffff'],
  },
};

export async function terminalPalettes(filename) {
  if (!filename) return defaultPalettes;
  const data = JSON.parse(await readFile(filename, 'utf8'));
  return Object.fromEntries(['light', 'dark'].map((mode) => {
    const { background, foreground, cursor, ansi } = data?.[mode] || {};
    if (!Array.isArray(ansi) || ansi.length !== 16
        || ![background, foreground, cursor, ...ansi].every((color) => typeof color === 'string' && hexColor.test(color))) {
      throw new Error('The terminal palette needs a background, foreground, cursor and 16 hex ANSI colors for both light and dark.');
    }
    return [mode, { background, foreground, cursor, ansi }];
  }));
}

export function paletteStyles(palettes) {
  const root = (mode) => {
    const { background, foreground, cursor, ansi } = palettes[mode];
    const colors = {
      ...sidebarPalettes[mode],
      'light-ink': mode === 'dark' ? light.active : light.paper,
      'brand-tile': mode === 'dark' ? light.raised : light.paper,
      terminal: background, 'terminal-ink': foreground, 'terminal-caret': cursor,
    };
    const declarations = Object.entries(colors).map(([role, color]) => '--' + role + ': ' + color + ';');
    ansi.forEach((color, index) => declarations.push('--terminal-ansi-' + index + ': ' + color + ';'));
    return ':root { color-scheme: ' + mode + ';\n  ' + declarations.join('\n  ') + '\n}';
  };
  return root('light') + '\n@media (prefers-color-scheme: dark) {\n' + root('dark') + '\n}';
}

function tmuxColor(value) {
  if (typeof value !== 'string') return undefined;
  if (hexColor.test(value)) return value;
  const color = value.toLowerCase();
  const indexed = /^colou?r(\d{1,3})$/.exec(color);
  if (indexed) return Number(indexed[1]) <= 255 ? Number(indexed[1]) : undefined;
  const base = ansiNames.indexOf(color.replace(/^bright-?/, ''));
  return base < 0 ? undefined : base + (color.startsWith('bright') ? 8 : 0);
}

export function tmuxPalette(line) {
  const [appearance, mode, accent, statusStyle, ...fields] = line.trimEnd().split('|');
  if (appearance !== 'light' && appearance !== 'dark') return undefined;
  const raw = fields.join('|');
  let palette;
  if (raw) {
    try { palette = JSON.parse(raw); } catch { return undefined; }
    if (palette?.version !== 1) return undefined;
  } else {
    const style = (statusStyle || '').split(',').map((part) => part.trim());
    palette = {
      background: style.find((part) => part.startsWith('bg='))?.slice(3),
      foreground: style.find((part) => part.startsWith('fg='))?.slice(3),
      accent,
    };
  }
  const roles = {};
  for (const name of tmuxRoles) {
    const color = tmuxColor(palette[name]);
    if (color !== undefined) roles[name] = color;
  }
  return { appearance, full: mode === 'full', roles };
}
