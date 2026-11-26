import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readdir, rm, stat, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { imageType } from './file-preview.mjs';
import { Failure, workbenchState } from './state.mjs';

const maxImages = 3;
const maxImageBytes = 256 * 1024;
export const promptBodyLimit = 1100 * 1024;
const directory = join(workbenchState, 'web-images');
const lifetime = 2 * 24 * 60 * 60 * 1000;
let checkedAt = 0;

export function decodePromptImages(value) {
  if (value === undefined) return [];
  if (!Array.isArray(value) || value.length > maxImages) throw new Failure(400, 'Attach up to three images.');
  return value.map((image) => {
    const { content, mime } = image || {};
    if (typeof content !== 'string' || !content || content.length > Math.ceil(maxImageBytes / 3) * 4
        || !/^[A-Za-z0-9+/]+={0,2}$/.test(content)) throw new Failure(400, 'Invalid image attachment.');
    const bytes = Buffer.from(content, 'base64');
    if (bytes.length > maxImageBytes || bytes.toString('base64') !== content
        || !mime || imageType(bytes) !== mime) throw new Failure(400, 'Invalid image attachment.');
    return { bytes, mime };
  });
}

export function imageFingerprint(images) {
  return images.map(({ bytes }) => createHash('sha256').update(bytes).digest('hex')).join('/');
}

export async function stagePromptImages(images) {
  if (!images.length) return [];
  await mkdir(directory, { recursive: true, mode: 0o700 });
  const folder = await mkdtemp(join(directory, 'upload-'));
  try {
    const paths = [];
    for (const [index, { bytes, mime }] of images.entries()) {
      const extension = mime === 'image/jpeg' ? 'jpg' : mime.slice('image/'.length);
      const path = join(folder, (index + 1) + '.' + extension);
      await writeFile(path, bytes, { mode: 0o600, flag: 'wx' });
      paths.push(path);
    }
    return paths;
  } catch (error) {
    await rm(folder, { recursive: true, force: true }).catch(() => {});
    throw error;
  }
}

export function prunePromptImages() {
  const now = Date.now();
  if (now - checkedAt < 60 * 60 * 1000) return;
  checkedAt = now;
  readdir(directory, { withFileTypes: true }).then(async (entries) => {
    for (const entry of entries) {
      if (!entry.isDirectory() || !/^upload-[A-Za-z0-9]{6}$/.test(entry.name)) continue;
      const path = join(directory, entry.name);
      const modified = await stat(path).catch(() => undefined);
      if (modified && now - modified.mtimeMs > lifetime) await rm(path, { recursive: true, force: true }).catch(() => {});
    }
  }).catch(() => {});
}
