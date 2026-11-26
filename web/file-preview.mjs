import { constants } from 'node:fs';
import { open, realpath, stat } from 'node:fs/promises';
import { basename, isAbsolute, relative, resolve, sep } from 'node:path';
import { Failure } from './state.mjs';

// Base64 stays within the registered node connection's reply limit.
const maxBytes = 768 * 1024;
const png = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);

function inside(root, path) {
  const suffix = relative(root, path);
  return suffix !== '..' && !suffix.startsWith('..' + sep) && !isAbsolute(suffix);
}

function fileType(filename, bytes) {
  if (bytes.subarray(0, 8).equals(png)) return 'image/png';
  if (bytes[0] === 255 && bytes[1] === 216 && bytes[2] === 255) return 'image/jpeg';
  if (/^GIF8[79]a$/.test(bytes.toString('ascii', 0, 6))) return 'image/gif';
  if (bytes.toString('ascii', 0, 4) === 'RIFF' && bytes.toString('ascii', 8, 12) === 'WEBP') return 'image/webp';
  try {
    if (bytes.includes(0)) throw new Error();
    new TextDecoder('utf-8', { fatal: true }).decode(bytes);
  } catch { throw new Failure(415, 'Preview supports UTF-8 text and PNG, JPEG, GIF, or WebP images.'); }
  return /\.(?:md|markdown)$/i.test(filename) ? 'text/markdown' : 'text/plain';
}

export async function readWorkspaceFile(path, cwd, repository = cwd) {
  if (typeof path !== 'string' || !path || path.length > 8192 || /[\x00-\x1f\x7f]/.test(path)) {
    throw new Failure(400, 'Invalid file path.');
  }
  const outside = () => new Failure(403, 'This file is outside the Agent workspace.');
  const missing = () => new Failure(404, 'The linked file is unavailable on the Agent machine.');
  try {
    const directory = await realpath(cwd);
    const checkout = await realpath(repository).catch(() => directory);
    const root = inside(checkout, directory) ? checkout : directory;
    const requested = resolve(cwd, path);
    const canonical = await realpath(requested);
    if (!inside(root, canonical)) throw outside();
    if (!(await stat(canonical)).isFile()) throw new Failure(415, 'Preview requires a regular file.');
    const file = await open(canonical, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
    try {
      const initial = await file.stat();
      if (!initial.isFile()) throw new Failure(415, 'Preview requires a regular file.');
      if (initial.size > maxBytes) throw new Failure(413, 'The linked file is too large to preview.');
      const current = await realpath(requested);
      if (!inside(root, current)) throw outside();
      const target = await stat(current);
      if (target.dev !== initial.dev || target.ino !== initial.ino) throw missing();
      const buffer = Buffer.alloc(initial.size + 1);
      let read = 0;
      while (read < buffer.length) {
        const { bytesRead } = await file.read(buffer, read, buffer.length - read, read);
        if (!bytesRead) break;
        read += bytesRead;
      }
      if (read > initial.size) throw new Failure(409, 'The linked file changed. Try opening it again.');
      const bytes = buffer.subarray(0, read);
      return { path: current, name: basename(current), mime: fileType(current, bytes), content: bytes.toString('base64') };
    } finally { await file.close(); }
  } catch (error) {
    if (error instanceof Failure) throw error;
    if (['EACCES', 'EPERM'].includes(error.code)) throw new Failure(403, 'The linked file could not be read.');
    throw missing();
  }
}
