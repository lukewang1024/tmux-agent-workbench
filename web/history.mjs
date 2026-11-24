import { open, readdir } from 'node:fs/promises';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { Failure } from './state.mjs';

const maxRead = 4 * 1024 * 1024;
const idPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

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

function visibleMessage(row) {
  const content = row?.payload;
  if (row?.type !== 'response_item' || content?.type !== 'message'
      || !['user', 'assistant'].includes(content.role)) return null;
  const text = (Array.isArray(content.content) ? content.content : [])
    .filter((part) => ['input_text', 'output_text'].includes(part.type) && typeof part.text === 'string')
    .map((part) => part.text)
    .filter((part) => !/^# AGENTS\.md instructions\b|^<environment_context>|^<permissions instructions>/.test(part))
    .join('\n').trim();
  return text ? { role: content.role, text: text.slice(0, 32_000), time: row.timestamp, phase: content.phase } : null;
}

export class History {
  filenames = new Map();

  constructor(codexHome = process.env.CODEX_HOME || join(homedir(), '.codex')) {
    this.root = codexHome;
  }

  async locate(id) {
    if (!hasThread(id)) throw new Failure(409, 'The running agent has no verified Codex thread ID.');
    let filename = this.filenames.get(id);
    if (!filename) {
      const suffix = '-' + id + '.jsonl';
      filename = await findRollout(join(this.root, 'sessions'), suffix, 3)
        || await findRollout(join(this.root, 'archived_sessions'), suffix, 0);
      if (!filename) throw new Failure(404, 'Codex has not written history for this thread.');
      if (this.filenames.size >= 100) this.filenames.delete(this.filenames.keys().next().value);
      this.filenames.set(id, filename);
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
      const head = Buffer.alloc(64 * 1024);
      const read = await file.read(head, 0, head.length, 0);
      const newline = head.subarray(0, read.bytesRead).indexOf(10);
      const meta = newline < 0 ? null : decode(head.subarray(0, newline));
      if (meta?.type !== 'session_meta' || meta.payload?.id !== id) {
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
