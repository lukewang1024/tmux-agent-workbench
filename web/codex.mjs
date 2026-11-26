import { execFile, spawn } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import { createInterface } from 'node:readline';
import { promisify } from 'node:util';
import { Failure } from './state.mjs';

const modelId = /^[a-z0-9][a-z0-9._:/-]{0,127}$/i;
const effortId = /^(?:none|minimal|low|medium|high|xhigh|max|ultra)$/;
const profileId = /^:?[a-z0-9][a-z0-9._-]{0,127}$/i;
const run = promisify(execFile);
const plain = (line) => line.replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, '');
const prompt = /^\s*› ?/u;

function queryCodex(env, method, params, failure) {
  return new Promise((resolve, reject) => {
    const child = spawn('codex', ['app-server'], { env, stdio: ['pipe', 'pipe', 'ignore'] });
    const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
    let settled = false;
    const timer = setTimeout(() => finish(), 6500);
    function finish(result) {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      lines.close();
      child.stdin.destroy();
      child.kill();
      if (result) resolve(result);
      else reject(new Failure(502, failure));
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
        write({ id: 2, method, params });
      } else if (message?.id === 2) finish(message.result);
      else if (message?.id === 1) finish();
    });
    write({ id: 1, method: 'initialize', params: {
      clientInfo: { name: 'workbench-web', version: '1.0.0' }, capabilities: { experimentalApi: true },
    } });
  });
}

export async function readModels(env) {
  const error = 'Could not load the Codex model list.';
  const { data } = await queryCodex(env, 'model/list', { limit: 64 }, error);
  const models = new Map();
  for (const item of Array.isArray(data) ? data : []) {
    if (typeof item?.model !== 'string' || !modelId.test(item.model) || item.hidden) continue;
    const efforts = (Array.isArray(item.supportedReasoningEfforts) ? item.supportedReasoningEfforts : [])
      .map((option) => option?.reasoningEffort).filter((effort) => effortId.test(effort));
    models.set(item.model, {
      id: item.model, label: String(item.displayName || item.model).slice(0, 100),
      efforts: [...new Set(efforts)], defaultEffort: efforts.includes(item.defaultReasoningEffort)
        ? item.defaultReasoningEffort : efforts[0] || null,
    });
  }
  if (!models.size) throw new Failure(502, error);
  return [...models.values()];
}

export async function readSkills(env, cwd) {
  const { data } = await queryCodex(env, 'skills/list', { cwds: [cwd], forceReload: true }, 'Could not load Codex skills.');
  const skills = (Array.isArray(data) ? data : []).flatMap((entry) => Array.isArray(entry.skills) ? entry.skills : []);
  const enabled = new Map();
  for (const item of skills) {
    if (!item?.enabled || typeof item.name !== 'string' || !modelId.test(item.name)) continue;
    enabled.set(item.name, {
      name: item.name,
      label: String(item.interface?.displayName || item.name).slice(0, 100),
      description: String(item.interface?.shortDescription || item.shortDescription || item.description || '').slice(0, 180),
    });
  }
  return [...enabled.values()];
}

export async function readPermissions(env, cwd) {
  const { data } = await queryCodex(env, 'permissionProfile/list', { cwd }, 'Could not load Codex permission profiles.');
  return (Array.isArray(data) ? data : []).filter((item) =>
    item?.allowed && typeof item.id === 'string' && profileId.test(item.id)
  ).map(({ id, description }) => ({ id, description: String(description || '').slice(0, 180) }));
}

const permissionPresets = [
  { id: 'mode:ask', mode: 'askForApproval', profile: ':workspace', sandbox: 'workspace-write', policy: 'on-request', reviewer: 'user' },
  { id: 'mode:approve', mode: 'approveForMe', profile: ':workspace', sandbox: 'workspace-write', policy: 'on-request', reviewer: 'auto_review' },
  { id: 'mode:yolo', mode: 'yolo', profile: ':danger-full-access', sandbox: 'danger-full-access', policy: 'never', reviewer: 'user' },
];

export function permissionChoices(profiles, requirements) {
  const allowed = (values, value) => values == null || values.includes(value);
  const presets = permissionPresets.filter(({ profile, sandbox, policy, reviewer }) =>
    profiles.some(({ id }) => id === profile)
    && allowed(requirements?.allowedSandboxModes, sandbox)
    && allowed(requirements?.allowedApprovalPolicies, policy)
    && allowed(requirements?.allowedApprovalsReviewers, reviewer)
  ).map(({ id, mode, profile, policy, reviewer }) => ({
    id, mode, arguments: profile + ' --approval-policy=' + policy + ' --reviewer=' + reviewer,
  }));
  if (!presets.length) return profiles.map((profile) => ({ ...profile, arguments: profile.id }));
  return [...presets, ...profiles.filter(({ id }) => !id.startsWith(':')).map((profile) => ({ ...profile, arguments: profile.id }))];
}

export async function readPermissionChoices(env, cwd) {
  const [profiles, config] = await Promise.all([
    readPermissions(env, cwd),
    queryCodex(env, 'configRequirements/read', {}, 'Could not load Codex permission requirements.').catch(() => null),
  ]);
  return config ? permissionChoices(profiles, config.requirements) : profiles.map((profile) => ({ ...profile, arguments: profile.id }));
}

export function codexCommands(screen, cursorY) {
  const items = [];
  for (const row of screen.split('\n').slice(0, cursorY).map(plain).reverse()) {
    const match = row.match(/^\s*(›\s*)?\/([a-z][a-z0-9-]*)\s{2,}(\S.+)$/i);
    if (match) items.unshift({ name: match[2], label: '/' + match[2], description: match[3].trim(), selected: Boolean(match[1]) });
    else if (items.length && !/^\s{4,}\S/.test(row)) break;
  }
  return items;
}

export function codexOutput(before, after) {
  const lines = (screen) => screen.split('\n').map((line) => plain(line).trim()).filter(Boolean);
  const previous = new Set(lines(before));
  return lines(after).filter((line) => !previous.has(line) && !prompt.test(line)).slice(-18).join('\n').slice(0, 3000);
}

export function codexFullAccessConfirmation(screen) {
  const text = plain(screen);
  return /^\s*›\s*1\. Yes, continue anyway\s+Apply full access for this session\s*$/m.test(text)
    && /^\s*2\. Cancel\s+Go back without enabling full access\s*$/m.test(text)
    && /enter select\s*·\s*esc back\s*$/m.test(text);
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
    for (const segment of line.split(/\s+[·•]\s+/)) {
      const label = normalized(segment.replace(/\s+(?:⚡.*|ultrafast|fast)$/iu, ''));
      for (const model of models) {
        for (const name of [model.id, model.label]) {
          const prefix = normalized(name);
          if (label === prefix) return { model: model.id, effort: null };
          if (label.startsWith(prefix + '-')) {
            const effort = label.slice(prefix.length + 1);
            if (effort === 'default' || effortId.test(effort)) return { model: model.id, effort };
          }
        }
      }
    }
  }
  return { model: null, effort: null };
}

export function codexRuntimeSettings(settings) {
  const model = typeof settings.model === 'string' && modelId.test(settings.model) ? settings.model : null;
  const effort = settings.reasoning_effort ?? settings.effort;
  const policy = settings.approval_policy;
  const profile = settings.active_permission_profile?.id;
  const id = typeof profile === 'string' && profileId.test(profile) ? profile : null;
  const sandbox = settings.sandbox_policy?.type ?? settings.sandbox_policy;
  const unrestricted = settings.permission_profile?.type === 'disabled' || sandbox === 'danger-full-access';
  const fileSystem = settings.permission_profile?.file_system;
  const readOnly = id === ':read-only' || sandbox === 'read-only'
    || (fileSystem?.type === 'restricted' && Array.isArray(fileSystem.entries) && fileSystem.entries.length
      && fileSystem.entries.every((entry) => entry?.access === 'read'));
  let mode;
  if (unrestricted) mode = policy === 'never' ? 'yolo' : 'fullAccess';
  else if (policy && policy !== 'never' && ['auto_review', 'guardian'].includes(settings.approvals_reviewer)) mode = 'approveForMe';
  else if (readOnly) mode = 'readOnly';
  else if (id && id !== ':workspace') mode = 'custom';
  else if (policy === 'never') mode = 'noApprovals';
  else if (policy) mode = 'askForApproval';
  return { model, effort: effortId.test(effort) ? effort : null, permission: mode ? { mode, id } : null };
}

export async function codexLaunchPermission(pid) {
  if (!Number.isInteger(pid) || pid <= 0) return null;
  let args;
  if (process.platform === 'linux') {
    args = (await readFile('/proc/' + pid + '/cmdline', 'utf8').catch(() => '')).split('\0');
  } else if (process.platform === 'darwin') {
    const { stdout } = await run('/bin/ps', ['-ww', '-o', 'command=', '-p', String(pid)], {
      timeout: 1500, maxBuffer: 64 * 1024,
    }).catch(() => ({ stdout: '' }));
    args = stdout.trim().split(/\s+/);
  } else return null;
  const end = args.indexOf('--');
  const flags = args.slice(1, end < 0 ? undefined : end);
  const yolo = flags.includes('--yolo') || flags.includes('--dangerously-bypass-approvals-and-sandbox');
  const review = flags.includes('--approve-for-me') || flags.includes('--not-so-yolo');
  if (yolo === review) return null;
  return { mode: yolo ? 'yolo' : 'approveForMe', id: null, launch: true };
}

export function modelChangeConfirmed(screen, model, effort) {
  const message = 'Model changed to ' + model + (effort ? ' ' + effort : '');
  return screen.split('\n').map(plain).filter((line) => {
    const text = line.trim().replace(/^•\s*/, '');
    return text === message || text.startsWith(message + ' for this session');
  }).length;
}
