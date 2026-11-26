import { readPublicDocument } from './public-document.mjs';
import { Failure } from './state.mjs';

const origin = 'https://developer.android.google.cn';

export function androidDocURL(path) {
  if (typeof path !== 'string' || path.length > 1024
      || !/^\/(?:[a-z\d][a-z\d_.-]*\/)*[a-z\d][a-z\d_.-]*\/?$/i.test(path)) {
    throw new Failure(400, 'Invalid Android documentation path.');
  }
  const url = new URL(path.replace(/\/$/, ''), origin);
  url.searchParams.set('hl', 'en');
  return url;
}

export function readAndroidDoc(path) {
  return readPublicDocument(androidDocURL(path), { mime: 'text/markdown', limit: 512 * 1024 });
}
