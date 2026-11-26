import { readFile } from 'node:fs/promises';

const htmlEntities = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };

export function resolveLanguage(env = process.env) {
  const override = [env.LOCALE, env.TMUX_AGENT_WORKBENCH_LANGUAGE]
    .map((value) => value?.trim())
    .find((value) => value && value.toLowerCase() !== 'auto');
  const category = env.LC_ALL || env.LC_MESSAGES || env.LANG || 'C';
  const locale = override || (/^(C|POSIX)(\.|$)/i.test(category) ? category : env.LANGUAGE || category);
  return /^zh([-_.@:]|$)/i.test(locale.trim()) ? 'zh-CN' : 'en';
}

async function readCatalog(language) {
  return JSON.parse(await readFile(new URL('locales/' + language + '.json', import.meta.url), 'utf8'));
}

export async function loadLocale() {
  const language = resolveLanguage();
  const english = await readCatalog('en');
  const catalog = language === 'en' ? english : await readCatalog(language);
  return { language, messages: { ...english.messages, ...catalog.messages }, errors: catalog.errors || {} };
}

export function renderDocument(template, { language, messages }) {
  return template.replace(/\{\{(\w+)\}\}/g, (_, key) => {
    const value = key === 'language' ? language : messages[key];
    if (typeof value !== 'string') throw new Error('Invalid message in HTML template: ' + key);
    return value.replace(/[&<>"']/g, (character) => htmlEntities[character]);
  });
}
