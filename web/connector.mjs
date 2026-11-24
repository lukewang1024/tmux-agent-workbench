import { randomUUID } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';
import { WebSocket } from 'ws';
import { Failure, parseOrigin } from './state.mjs';

export const nodeProtocol = '1';
const headers = (id, credential) => ({
  'x-workbench-node': id,
  'x-workbench-protocol': nodeProtocol,
  authorization: 'Bearer ' + credential,
});

export async function enrollNode(gateway, invite) {
  const origin = parseOrigin(gateway).origin;
  const response = await fetch(origin + '/node/enroll', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ invite }),
    redirect: 'error',
    signal: AbortSignal.timeout(10_000),
  });
  if (!response.ok) throw new Failure(response.status, 'Node pairing failed. Check the gateway and invitation.');
  return { ...await response.json(), gateway: origin };
}

function send(socket, value) {
  if (socket.readyState !== WebSocket.OPEN || socket.bufferedAmount > 2 * 1024 * 1024) {
    throw new Failure(503, 'Connection is unavailable.');
  }
  socket.send(JSON.stringify(value));
}

export class RemoteNode {
  pending = new Map();

  constructor(socket, removed) {
    this.socket = socket;
    socket.on('message', (data) => {
      try {
        const message = JSON.parse(data);
        const record = this.pending.get(message.id);
        if (!record) return;
        this.pending.delete(message.id);
        clearTimeout(record.timer);
        if (message.error) {
          const { status, message: reason } = message.error;
          const code = Number.isInteger(status) && status >= 400 && status <= 599 ? status : 502;
          record.reject(new Failure(code, typeof reason === 'string' ? reason.slice(0, 300) : 'The node operation failed.'));
        } else record.resolve(message.result);
      } catch {
        socket.close(1002, 'Invalid node reply');
      }
    });
    socket.on('error', () => {});
    socket.on('close', () => {
      for (const record of this.pending.values()) {
        clearTimeout(record.timer);
        record.reject(new Failure(503, 'The node disconnected.'));
      }
      this.pending.clear();
      removed();
    });
  }

  request(method, args = {}) {
    if (this.pending.size >= 64) return Promise.reject(new Failure(429, 'The node is busy.'));
    return new Promise((resolve, reject) => {
      const id = randomUUID();
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Failure(504, 'The node did not respond.'));
      }, method === 'usage' ? 25_000 : 10_000);
      this.pending.set(id, { resolve, reject, timer });
      try {
        send(this.socket, { id, method, args });
      } catch (error) {
        clearTimeout(timer);
        this.pending.delete(id);
        reject(error);
      }
    });
  }
}

export async function runConnector(config, local, signal) {
  const gateway = parseOrigin(config.gateway);
  const protocol = gateway.protocol === 'https:' ? 'wss:' : 'ws:';
  const destination = protocol + '//' + gateway.host + '/node/control';
  let backoff = 500;
  while (!signal.aborted) {
    const connected = await new Promise((resolve) => {
      const socket = new WebSocket(destination, {
        headers: headers(config.id, config.credential),
        maxPayload: 96 * 1024, handshakeTimeout: 10_000, perMessageDeflate: false,
      });
      let online = false;
      let inFlight = 0;
      let lastPing = Date.now();
      const stop = () => socket.terminate();
      signal.addEventListener('abort', stop, { once: true });
      const timer = setInterval(() => {
        local.cleanup();
        if (Date.now() - lastPing > 35_000) socket.terminate();
      }, 5000);
      socket.on('ping', () => { lastPing = Date.now(); });
      socket.on('open', () => {
        online = true;
        process.stderr.write('Workbench node connected: ' + config.id + '\n');
      });
      socket.on('error', () => {});
      socket.on('close', () => {
        clearInterval(timer);
        signal.removeEventListener('abort', stop);
        local.close();
        resolve(online);
      });
      socket.on('message', async (data) => {
        let packet;
        try { packet = JSON.parse(data); } catch { socket.close(1002, 'Invalid request'); return; }
        if (typeof packet?.id !== 'string' || typeof packet.method !== 'string' || !packet.args) return;
        if (inFlight >= 32) {
          try { send(socket, { id: packet.id, error: { status: 429, message: 'The node is busy.' } }); } catch {}
          return;
        }
        inFlight++;
        try {
          const result = await local.request(packet.method, packet.args);
          send(socket, { id: packet.id, result });
        } catch (error) {
          try {
            send(socket, { id: packet.id, error: error instanceof Failure
              ? { status: error.status, message: error.message }
              : { status: 502, message: 'Workbench could not complete the node operation.' } });
          } catch {}
        } finally {
          inFlight--;
        }
      });
    });
    if (connected) backoff = 500;
    try { await delay(backoff + Math.random() * 250, undefined, { signal }); } catch { break; }
    backoff = Math.min(backoff * 2, 15_000);
  }
}
