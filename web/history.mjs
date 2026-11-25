import { execFile } from 'node:child_process';
import { open, readdir, readlink, realpath } from 'node:fs/promises';
import { homedir } from 'node:os';
import { basename, join, sep } from 'node:path';
import { promisify } from 'node:util';
import { Failure } from './state.mjs';

const maxRead = 4 * 1024 * 1024;
const idPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const rolloutPattern = /^rollout-.+-([0-9a-f-]{36})\.jsonl$/i;
const run = promisify(execFile);
const goalContext = /^\s*<codex_internal_context\s+source="(?:user_goal|goal)"\s*>/;
const contextEnd = '</codex_internal_context>';

export function hasThread(id) {
  return typeof id === 'string' && idPattern.test(id);
}

async function findRollout(directory, suffix, depth) {
  let entries;
  try {
    entries = await readdir(directory, { withFileTypes: true });
  } catch (error) {
    if (error.code === 'ENOENT') return null;
    throw error;
  }
  const entry = entries.find((item) => item.isFile() && item.name.endsWith(suffix));
  if (entry) return join(directory, entry.name);
  if (depth) {
    for (const item of entries.sort((a, b) => b.name.localeCompare(a.name))) {
      if (!item.isDirectory() || !/^\d{2,4}$/.test(item.name)) continue;
      const found = await findRollout(join(directory, item.name), suffix, depth - 1);
      if (found) return found;
    }
  }
  return null;
}

function decode(line) {
  try { return JSON.parse(line.toString('utf8')); } catch { return null; }
}

async function rolloutId(file) {
  const head = Buffer.alloc(64 * 1024);
  const { bytesRead } = await file.read(head, 0, head.length, 0);
  const newline = head.subarray(0, bytesRead).indexOf(10);
  const meta = newline < 0 ? null : decode(head.subarray(0, newline));
  return meta?.type === 'session_meta' ? meta.payload?.id : null;
}

async function openRollouts(pids) {
  const files = new Map(pids.map((pid) => [pid, []]));
  if (process.platform === 'linux') {
    await Promise.all(pids.map(async (pid) => {
      const directory = '/proc/' + pid + '/fd';
      const descriptors = await readdir(directory).catch(() => []);
      const names = await Promise.all(descriptors.map((fd) => readlink(join(directory, fd)).catch(() => '')));
      files.set(pid, names.filter((name) => rolloutPattern.test(basename(name)) && name.startsWith(sep)));
    }));
  } else if (process.platform === 'darwin') {
    // macOS has no procfs. Limit lsof to the Workbench-identified processes.
    const { stdout } = await run('/usr/sbin/lsof', ['-nP', '-a', '-p', pids.join(','), '-Fpn0'], {
      timeout: 2000, maxBuffer: 2 * 1024 * 1024,
    }).catch(() => ({ stdout: '' }));
    let paths;
    for (const value of stdout.split('\0')) {
      const field = value.trimStart();
      if (field.startsWith('p')) paths = files.get(Number(field.slice(1)));
      if (paths && field.startsWith('n' + sep) && rolloutPattern.test(basename(field.slice(1)))) paths.push(field.slice(1));
    }
  }
  return files;
}

async function startTimes(pids) {
  const { stdout } = await run('/bin/ps', ['-o', 'pid=,lstart=', '-p', pids.join(',')], {
    env: { ...process.env, LC_ALL: 'C', TZ: 'UTC' }, timeout: 1500,
  }).catch(() => ({ stdout: '' }));
  const times = new Map();
  for (const line of stdout.split('\n')) {
    const match = line.trim().match(/^(\d+)\s+(.+)$/);
    if (match) times.set(Number(match[1]), Date.parse(match[2] + ' UTC') / 1000);
  }
  return times;
}

export function stripGoalContext(text) {
  let visible = text;
  for (let start; (start = visible.match(goalContext)); ) {
    const end = visible.indexOf(contextEnd, start[0].length);
    if (end < 0) break;
    visible = visible.slice(end + contextEnd.length);
  }
  return visible === text ? text : visible.replace(/^(?:[ \t]*\r?\n)+/, '');
}

function visibleMessage(row) {
  const content = row?.payload;
  if (row?.type !== 'response_item' || content?.type !== 'message'
      || !['user', 'assistant'].includes(content.role)) return null;
  const text = (Array.isArray(content.content) ? content.content : [])
    .filter((part) => ['input_text', 'output_text'].includes(part?.type) && typeof part.text === 'string')
    .map((part) => content.role === 'user' ? stripGoalContext(part.text) : part.text)
    .filter((part) => !/^# AGENTS\.md instructions\b|^<environment_context>|^<permissions instructions>/.test(part))
    .join('\n').replace(/^(?:[ \t]*\r?\n)+|(?:\r?\n[ \t]*)+$/g, '');
  return text.trim() ? { role: content.role, text: text.slice(0, 32_000), time: row.timestamp, phase: content.phase } : null;
}

export class History {
  filenames = new Map();

  constructor(codexHome = process.env.CODEX_HOME || join(homedir(), '.codex')) {
    this.root = codexHome;
  }

  remember(id, filename) {
    this.filenames.set(id, filename);
    if (this.filenames.size > 100) this.filenames.delete(this.filenames.keys().next().value);
  }

  async activeThreads(agents) {
    const candidates = agents.filter(({ process }) => Number.isInteger(process?.pid) && process.pid > 0
      && Number.isSafeInteger(process.started_at_ticks) && process.started_at_ticks > 0);
    const threads = new Map();
    if (!candidates.length) return threads;
    const pids = [...new Set(candidates.map(({ process }) => process.pid))];
    const files = await openRollouts(pids);
    const observed = pids.filter((pid) => files.get(pid).length);
    if (!observed.length) return threads;
    // Compare the same start time as the native Unix process fingerprint before
    // using a descriptor from a snapshot's PID.
    const times = await startTimes(observed);
    const roots = await Promise.all(['sessions', 'archived_sessions'].map((name) => realpath(join(this.root, name)).catch(() => null)));
    for (const agent of candidates) {
      if (times.get(agent.process.pid) !== agent.process.started_at_ticks) continue;
      const hint = agent.hook_session_id;
      const matches = new Map();
      for (const name of new Set(files.get(agent.process.pid))) {
        const id = basename(name).match(rolloutPattern)?.[1];
        if (!hasThread(id) || (hint && (hint.length < 29 || !id.startsWith(hint)))) continue;
        const filename = await realpath(name).catch(() => null);
        if (!filename || !roots.some((root) => root && filename.startsWith(root + sep))) continue;
        const file = await open(filename, 'r').catch(() => null);
        if (!file) continue;
        try {
          if (await rolloutId(file) === id) matches.set(id, filename);
        } catch {
          // The process can replace or close the rollout during observation.
        } finally { await file.close(); }
      }
      if (matches.size !== 1) continue;
      const [[id, filename]] = matches;
      this.remember(id, filename);
      threads.set(agent.instance_id, id);
    }
    return threads;
  }

  async locate(id) {
    if (!hasThread(id)) throw new Failure(409, 'The running agent has no verified Codex thread ID.');
    let filename = this.filenames.get(id);
    if (!filename) {
      const suffix = '-' + id + '.jsonl';
      filename = await findRollout(join(this.root, 'sessions'), suffix, 3)
        || await findRollout(join(this.root, 'archived_sessions'), suffix, 0);
      if (!filename) throw new Failure(404, 'Codex has not written history for this thread.');
      this.remember(id, filename);
    }
    return filename;
  }

  async page(id, before) {
    const filename = await this.locate(id);
    let file;
    try {
      file = await open(filename, 'r');
    } catch (error) {
      if (error.code !== 'ENOENT') throw error;
      this.filenames.delete(id);
      throw new Failure(404, 'Codex history moved. Refresh to reopen it.');
    }
    try {
      if (await rolloutId(file) !== id) {
        throw new Failure(409, 'Codex history does not match the running thread.');
      }

      const size = (await file.stat()).size;
      const end = Number.isSafeInteger(before) && before > 0 ? Math.min(before, size) : size;
      const start = Math.max(0, end - maxRead);
      const block = Buffer.alloc(end - start);
      const { bytesRead } = await file.read(block, 0, block.length, start);
      const bytes = block.subarray(0, bytesRead);
      // A rollout may be in the middle of an append. Only parse complete records.
      let last = bytes.lastIndexOf(10);
      const first = start ? bytes.indexOf(10) + 1 : 0;
      const records = [];
      let cursor = start;
      while (last >= first && records.length < 35) {
        const previous = last > 0 ? bytes.lastIndexOf(10, last - 1) : -1;
        const lineStart = previous + 1;
        if (lineStart < first) break;
        cursor = start + lineStart;
        const message = visibleMessage(decode(bytes.subarray(lineStart, last)));
        if (message) records.push({ ...message, id: cursor });
        last = previous;
      }
      // Advance even if a block contained only tool data or part of a large record.
      const blockBoundary = first && start + first < end ? start + first : start;
      const next = last >= first && cursor > 0 ? cursor : blockBoundary;
      return { messages: records.reverse(), before: next || null };
    } finally {
      await file.close();
    }
  }
}
