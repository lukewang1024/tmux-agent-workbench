// Load before the stylesheet so saved preferences apply to the first paint.
const workbenchAppearance = (() => {
  const modes = ['system', 'on', 'off'];
  const media = {
    theme: matchMedia('(prefers-color-scheme: dark)'),
    motion: matchMedia('(prefers-reduced-motion: reduce)'),
  };
  const root = document.documentElement;
  const themeColor = document.getElementById('page-theme-color');
  const listeners = new Set();
  const preferences = {};
  const key = (setting) => 'workbench.appearance.' + setting;

  for (const setting of Object.keys(media)) {
    let value;
    try { value = localStorage.getItem(key(setting)); } catch {}
    preferences[setting] = modes.includes(value) ? value : 'system';
    media[setting].addEventListener('change', apply);
  }

  function enabled(setting) {
    const value = preferences[setting];
    return value === 'system' ? media[setting].matches : value === 'on';
  }

  function apply() {
    const theme = enabled('theme') ? 'dark' : 'light';
    const motion = enabled('motion') ? 'reduce' : 'full';
    const themeChanged = root.dataset.theme !== theme;
    if (!themeChanged && root.dataset.motion === motion) return;
    root.dataset.theme = theme;
    root.dataset.motion = motion;
    if (themeChanged) themeColor.content = themeColor.dataset[theme];
    for (const listener of listeners) listener({ themeChanged });
  }

  apply();
  return {
    modes,
    get: (setting) => preferences[setting],
    set(setting, value) {
      preferences[setting] = value;
      try { localStorage.setItem(key(setting), value); } catch {}
      apply();
    },
    subscribe: (listener) => listeners.add(listener),
  };
})();
