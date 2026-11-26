import { Agent, get } from 'node:https';
import { Failure } from './state.mjs';

const agent = new Agent({ proxyEnv: process.env });
const upstreamStatuses = new Set([403, 404, 408, 410, 429, 500, 502, 503, 504]);

// Callers supply only URLs admitted by their own host and path rules.
export async function readPublicDocument(url, { mime, limit, timeout = 8_000 }) {
  try {
    const response = await new Promise((resolve, reject) => {
      get(url, { agent, headers: { accept: mime }, signal: AbortSignal.timeout(timeout) }, resolve)
        .once('error', reject);
    });
    if (response.statusCode !== 200 || response.headers['content-type']?.split(';', 1)[0] !== mime) {
      response.destroy();
      const status = upstreamStatuses.has(response.statusCode) ? response.statusCode : 415;
      throw new Failure(status, 'Could not load the document preview.');
    }
    const chunks = [];
    let size = 0;
    for await (const chunk of response) {
      size += chunk.length;
      if (size > limit) throw new Failure(413, 'The document preview is too large.');
      chunks.push(chunk);
    }
    return Buffer.concat(chunks);
  } catch (error) {
    if (error instanceof Failure) throw error;
    const status = error.name === 'AbortError' || error.code === 'ETIMEDOUT' ? 504 : 424;
    throw new Failure(status, 'Could not load the document preview.');
  }
}
