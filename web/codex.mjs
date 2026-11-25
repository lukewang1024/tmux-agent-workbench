import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { Failure } from './state.mjs';

const modelId = /^[a-z0-9][a-z0-9._:/-]{0,127}$/i;
const effortId = /^(?:none|minimal|low|medium|high|xhigh|max|ultra)$/;
const modelUnavailable = 'Could not load the Codex model list.';
const plain = (line) => line.replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, '');
const prompt = /^\s*› ?/u;

export function readModels(env) {
  return new Promise((resolve, reject) => {
    const child = spawn('codex', ['app-server'], { env, stdio: ['pipe', 'pipe', 'ignore'] });
    const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
    let settled = false;
    const timer = setTimeout(() => finish(), 6500);
    function finish(models) {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      lines.close();
      child.stdin.destroy();
      child.kill();
      if (models?.length) resolve(models);
      else reject(new Failure(502, modelUnavailable));
    }
    const write = (message) => child.stdin.write(JSON.stringify({ jsonrpc: '2.0', ...message }) + '\n');
    child.stdin.on('error', () => finish());
    child.once('error', () => finish());
    child.once('close', () => finish());
    lines.on('line', (line) => {
      let message;
      try { message = JSON.parse(line); } catch { return; }
      if (message?.id === 1 && message.result) {
        write({ method: 'initialized' });
        write({ id: 2, method: 'model/list', params: { limit: 64 } });
      } else if (message?.id === 2) {
        const data = message.result?.data;
        if (!Array.isArray(data)) return finish();
        const models = new Map();
        for (const item of data) {
          if (typeof item?.model !== 'string' || !modelId.test(item.model) || item.hidden) continue;
          const efforts = (Array.isArray(item.supportedReasoningEfforts) ? item.supportedReasoningEfforts : [])
            .map((option) => option?.reasoningEffort).filter((effort) => effortId.test(effort));
          models.set(item.model, {
            id: item.model, label: String(item.displayName || item.model).slice(0, 100),
            efforts: [...new Set(efforts)], defaultEffort: efforts.includes(item.defaultReasoningEffort)
              ? item.defaultReasoningEffort : efforts[0] || null,
          });
        }
        finish([...models.values()]);
      } else if (message?.id === 1) finish();
    });
    write({ id: 1, method: 'initialize', params: { clientInfo: { name: 'workbench-web', version: '1.0.0' } } });
  });
}

export function codexComposer(screen, cursorY) {
  const rows = screen.split('\n').slice(0, cursorY + 1);
  const start = rows.findLastIndex((line) => prompt.test(plain(line)));
  if (start < 0) return null;
  const rendered = rows.slice(start).map(plain);
  rendered[0] = rendered[0].replace(prompt, '');
  const value = rendered.map((line) => line.trim()).join(' ').trim();
  const placeholder = start === cursorY && /^Ask Codex to do anything(?:\.{3}|…)?$/i.test(value)
    && /\x1b\[(?:[0-9;]*;)?2(?:;[0-9;]*)?m/.test(rows[start]);
  return placeholder ? '' : value;
}

export function codexSettings(screen, cursorY, models) {
  const lines = screen.split('\n').map(plain);
  const normalized = (value) => value.trim().toLowerCase().replace(/[\s-]+/g, '-');
  // The status line below the composer reflects the current session setting.
  for (const line of lines.slice(cursorY + 1).reverse()) {
    const label = normalized(line.split(/\s+[·•]\s+/)[0]);
    for (const model of models) {
      for (const name of [model.id, model.label]) {
        const prefix = normalized(name);
        if (label === prefix) return { model: model.id, effort: null };
        if (label.startsWith(prefix + '-')) {
          const effort = label.slice(prefix.length + 1);
          if (effortId.test(effort)) return { model: model.id, effort };
        }
      }
    }
  }
  return { model: null, effort: null };
}

export function modelChangeConfirmed(screen, model, effort) {
  const message = 'Model changed to ' + model + (effort ? ' ' + effort : '');
  return screen.split('\n').map(plain).filter((line) => {
    const text = line.trim().replace(/^•\s*/, '');
    return text === message || text.startsWith(message + ' for this session');
  }).length;
}
