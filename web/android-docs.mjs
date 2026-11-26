import { Agent, get } from 'node:https';
import { Failure } from './state.mjs';

const origin = 'https://developer.android.google.cn';
const agent = new Agent({ proxyEnv: process.env });

export function androidDocURL(path) {
  if (typeof path !== 'string' || path.length > 1024
      || !/^\/(?:[a-z\d][a-z\d_.-]*\/)*[a-z\d][a-z\d_.-]*\/?$/i.test(path)) {
    throw new Failure(400, 'Invalid Android documentation path.');
  }
  const url = new URL(path.replace(/\/$/, ''), origin);
  url.searchParams.set('hl', 'en');
  return url;
}

export async function readAndroidDoc(path) {
  const url = androidDocURL(path);
  try {
    const response = await new Promise((resolve, reject) => {
      get(url, { agent, headers: { accept: 'text/markdown' }, signal: AbortSignal.timeout(8_000) }, resolve)
        .once('error', reject);
    });
    if (response.statusCode !== 200 || response.headers['content-type']?.split(';', 1)[0] !== 'text/markdown') {
      response.destroy();
      throw new Failure(response.statusCode === 404 ? 404 : 502, 'Could not load the Android documentation preview.');
    }
    const chunks = [];
    let size = 0;
    for await (const chunk of response) {
      size += chunk.length;
      if (size > 512 * 1024) throw new Failure(413, 'The Android documentation preview is too large.');
      chunks.push(chunk);
    }
    return Buffer.concat(chunks);
  } catch (error) {
    if (error instanceof Failure) throw error;
    throw new Failure(502, 'Could not load the Android documentation preview.');
  }
}
