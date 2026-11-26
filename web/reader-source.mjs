import { readPublicDocument } from './public-document.mjs';
import { readerURL, webDevMirror } from './public/link-policy.js';
import { Failure } from './state.mjs';

export async function readReaderSource(value) {
  let source;
  try { source = readerURL(new URL(value)); } catch { /* Invalid or absent URL. */ }
  if (!source) throw new Failure(400, 'This address is not available in reader view.');
  if (!source.searchParams.has('hl')) source.searchParams.set('hl', 'en');
  const limits = { mime: 'text/html', limit: 2 * 1024 * 1024 };
  if (source.hostname !== webDevMirror) {
    try { return await readPublicDocument(source, { ...limits, timeout: 3_000 }); }
    catch (error) { if (error.status !== 502) throw error; }
    source.hostname = webDevMirror;
  }
  return readPublicDocument(source, { ...limits, timeout: 5_000 });
}
