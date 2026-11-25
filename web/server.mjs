import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { createServer as httpServer } from 'node:http';
import { createServer as httpsServer } from 'node:https';
import { isIP } from 'node:net';
import { promisify } from 'node:util';
import { WebSocket, WebSocketServer } from 'ws';
import { Auth, sameSecret } from './auth.mjs';
import { RemoteNode, nodeProtocol } from './connector.mjs';
import { loadLocale, renderDocument } from './i18n.mjs';
import { Failure, nodeName, parseOrigin } from './state.mjs';

const run = promisify(execFile);
const assets = new Map([
  ['/', ['public/index.html', 'text/html; charset=utf-8']],
  ['/app.js', ['public/app.js', 'text/javascript; charset=utf-8']],
  ['/style.css', ['public/style.css', 'text/css; charset=utf-8']],
  ['/manifest.webmanifest', ['public/manifest.webmanifest', 'application/manifest+json']],
  ['/sw.js', ['public/sw.js', 'text/javascript; charset=utf-8']],
  ['/icon-192.png', ['public/icon-192.png', 'image/png']],
  ['/icon-512.png', ['public/icon-512.png', 'image/png']],
  ['/vendor/passkeys.js', ['node_modules/@simplewebauthn/browser/dist/bundle/index.umd.min.js', 'text/javascript; charset=utf-8']],
  ['/vendor/xterm.js', ['node_modules/@xterm/xterm/lib/xterm.js', 'text/javascript; charset=utf-8']],
  ['/vendor/xterm.css', ['node_modules/@xterm/xterm/css/xterm.css', 'text/css; charset=utf-8']],
  ['/vendor/fit.js', ['node_modules/@xterm/addon-fit/lib/addon-fit.js', 'text/javascript; charset=utf-8']],
]);

function security(res) {
  res.setHeader('Cache-Control', 'no-store');
  res.setHeader('X-Content-Type-Options', 'nosniff');
  res.setHeader('Referrer-Policy', 'no-referrer');
  res.setHeader('Cross-Origin-Resource-Policy', 'same-origin');
  res.setHeader('X-Frame-Options', 'DENY');
  res.setHeader('Content-Security-Policy', "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'");
}

function json(res, result, status = 200) {
  res.writeHead(status, { 'content-type': 'application/json; charset=utf-8' });
  res.end(JSON.stringify(result));
}

async function body(req) {
  if (!/^application\/json(?:;|$)/i.test(req.headers['content-type'] || '')) {
    throw new Failure(415, 'Use application/json.');
  }
  const chunks = [];
  let length = 0;
  for await (const chunk of req) {
    length += chunk.length;
    if (length > 64 * 1024) throw new Failure(413, 'Request is too large.');
    chunks.push(chunk);
  }
  try {
    const value = JSON.parse(Buffer.concat(chunks).toString());
    if (!value || Array.isArray(value) || typeof value !== 'object') throw new Error();
    return value;
  } catch {
    throw new Failure(400, 'Invalid JSON request.');
  }
}

async function cachedAssets(locale) {
  const loaded = new Map();
  for (const [path, [file, type]] of assets) {
    const data = await readFile(new URL(file, import.meta.url));
    loaded.set(path, { data, type });
  }
  const { language, messages } = locale;
  const index = loaded.get('/');
  index.data = Buffer.from(renderDocument(index.data.toString(), locale));
  const script = loaded.get('/app.js');
  script.data = Buffer.from(script.data.toString().replace('WORKBENCH_LOCALE', () => JSON.stringify({ language, messages })));
  const manifest = loaded.get('/manifest.webmanifest');
  manifest.data = Buffer.from(JSON.stringify({ ...JSON.parse(manifest.data.toString()), lang: language, description: messages.description }));
  const fingerprint = createHash('sha256');
  for (const { data } of loaded.values()) fingerprint.update(data);
  loaded.get('/sw.js').data = Buffer.from(loaded.get('/sw.js').data.toString().replace('WORKBENCH_CACHE_VERSION', fingerprint.digest('hex').slice(0, 16)));
  return loaded;
}

export async function startServer({ local, localName, store, listen, origin, cert, key, fabric = [] }) {
  const url = parseOrigin(origin);
  if (isIP(url.hostname) || !/^[A-Za-z0-9][A-Za-z0-9.-]+$/.test(url.hostname)) throw new Failure(400, 'Use a DNS name for WebAuthn.');
  const binding = /^(?:127\.0\.0\.1|localhost):(\d+)$/.exec(listen);
  const port = Number(binding?.[1]);
  if (!binding || port > 65535) {
    throw new Failure(400, 'Use --listen 127.0.0.1:PORT or localhost:PORT.');
  }
  if (Boolean(cert) !== Boolean(key)) {
    throw new Failure(400, 'Provide both TLS files or neither.');
  }

  const locale = await loadLocale();
  const staticFiles = await cachedAssets(locale);
  const auth = new Auth(store, url);
  const remote = new Map();
  const leases = new Map();
  const browserSockets = new Set();
  const peers = new WebSocketServer({ noServer: true, maxPayload: 2 * 1024 * 1024, perMessageDeflate: false });
  const browser = new WebSocketServer({ noServer: true, maxPayload: 48 * 1024, perMessageDeflate: false });
  let fabricCache = { until: 0, task: null };

  function errorMessage(error, fallback) {
    const message = error instanceof Failure ? error.message : fallback;
    // English messages also come from connectors running earlier versions.
    return Object.hasOwn(locale.errors, message) ? locale.errors[message] : message;
  }

  async function fabricStatus() {
    if (!fabric.length) return new Map();
    if (fabricCache.until < Date.now()) {
      const task = run('machine-fabric', ['context'], { timeout: 3000, maxBuffer: 1_000_000 })
        .then(({ stdout }) => new Map((JSON.parse(stdout).result?.executors || []).map((item) => [item.executorId, item.health])))
        .catch(() => new Map());
      fabricCache = { until: Date.now() + 20_000, task };
    }
    return fabricCache.task;
  }

  function node(id) {
    if (id === 'local') return local;
    const peer = remote.get(id);
    if (!peer || !store.enrolled(id)) throw new Failure(503, 'The node is offline or has not been paired.');
    return peer;
  }

  async function workspace() {
    const inventory = new Map(store.listNodes().map((record) => [record.id, { id: record.id, name: record.id, state: remote.has(record.id) ? 'online' : record.connected ? 'offline' : 'pending' }]));
    for (const id of store.pendingNodes()) {
      if (!inventory.has(id)) inventory.set(id, { id, name: id, state: 'pending' });
    }
    const discovered = await fabricStatus();
    for (const [id, executor] of fabric) {
      if (!inventory.has(id)) inventory.set(id, { id, name: id, state: 'pending', fabric: discovered.get(executor) || 'unknown' });
    }
    const result = await Promise.all([
      { id: 'local', name: localName, state: 'online' }, ...inventory.values(),
    ].map(async (info) => {
      if (info.state !== 'online') return info;
      try {
        return { ...info, ...await node(info.id).request('catalog', {}) };
      } catch {
        return { ...info, snapshotAvailable: false };
      }
    }));
    return { nodes: result };
  }

  function authorized(req) {
    const session = auth.session(req, req.method !== 'GET');
    if (!session) throw new Failure(401, 'Sign in to Workbench.');
    return session;
  }

  async function handle(req, res) {
    security(res);
    const request = new URL(req.url, url);
    const path = request.pathname;
    const method = req.method;
    if (method === 'GET' && (staticFiles.has(path) || path === '/healthz')) {
      if (path === '/healthz') return json(res, { ok: true });
      const file = staticFiles.get(path);
      res.writeHead(200, { 'content-type': file.type, 'content-length': file.data.length });
      return res.end(file.data);
    }

    if (path === '/node/enroll' && method === 'POST') {
      if (req.headers.origin) throw new Failure(403, 'Node pairing requires the connector.');
      auth.throttle(req);
      return json(res, store.enroll((await body(req)).invite), 201);
    }
    if (method === 'POST' || method === 'DELETE') auth.writeOrigin(req);
    if (path === '/api/session' && method === 'GET') return json(res, { authenticated: Boolean(auth.session(req, false)) });
    if (path.startsWith('/api/auth/') && method === 'POST') {
      auth.throttle(req);
      if (path === '/api/auth/register/options') return json(res, await auth.registrationOptions(req, await body(req)));
      if (path === '/api/auth/register/finish') {
        const cookie = await auth.register(req, await body(req));
        if (cookie) res.setHeader('Set-Cookie', cookie);
        return json(res, {});
      }
      if (path === '/api/auth/login/options') return json(res, await auth.authenticationOptions());
      if (path === '/api/auth/login/finish') {
        res.setHeader('Set-Cookie', await auth.login(await body(req)));
        return json(res, {});
      }
    }

    if (!path.startsWith('/api/')) throw new Failure(404, 'Page not found.');
    const session = authorized(req);
    if (path === '/api/logout' && method === 'POST') {
      auth.sessions.delete(session);
      res.setHeader('Set-Cookie', auth.cookieHeader('', true));
      for (const client of browserSockets) if (client.session === session) client.socket.close(4001, 'Signed out');
      return json(res, {});
    }
    if (path === '/api/passkeys' && method === 'GET') return json(res, auth.passkeys());
    if (path === '/api/passkeys' && method === 'DELETE') {
      auth.revoke((await body(req)).id);
      for (const client of browserSockets) if (!auth.valid(client.session)) client.socket.close(4001, 'Passkey revoked');
      return json(res, {});
    }
    if (path === '/api/workspace' && method === 'GET') return json(res, await workspace());
    if (path === '/api/usage' && (method === 'GET' || method === 'POST')) {
      return json(res, await local.request('usage', { refresh: method === 'POST' }));
    }
    if (path === '/api/history' && method === 'GET') {
      const machine = node(request.searchParams.get('node') || 'local');
      const before = Number(request.searchParams.get('before'));
      return json(res, await machine.request('history', { agent: request.searchParams.get('agent'), before }));
    }
    if (path === '/api/prompt' && method === 'POST') {
      const input = await body(req);
      if (typeof input.pane !== 'string' || !/^\d+(?:\.\d+){3}$/.test(input.pane)) throw new Failure(400, 'Invalid Pane.');
      const lease = input.node + ':' + input.pane;
      if (leases.has(lease)) throw new Failure(409, 'Release terminal input before sending from chat.');
      const prompt = {};
      leases.set(lease, prompt);
      try {
        return json(res, await node(input.node).request('prompt', input));
      } finally {
        if (leases.get(lease) === prompt) leases.delete(lease);
      }
    }
    if (path === '/api/panes' && method === 'POST') {
      const input = await body(req);
      return json(res, await node(input.node).request('create', { split: input.split }), 201);
    }
    throw new Failure(404, 'Page not found.');
  }

  const handler = (req, res) => handle(req, res).catch((error) => {
    if (res.destroyed) return;
    if (res.headersSent) { res.destroy(); return; }
    const status = error instanceof Failure ? error.status : 502;
    json(res, { error: errorMessage(error, 'Workbench could not complete the operation.') }, status);
  });
  const server = cert ? httpsServer({ cert: await readFile(cert), key: await readFile(key) }, handler) : httpServer(handler);
  server.requestTimeout = 12_000;
  server.headersTimeout = 8000;

  function terminal(socket, req, request, session) {
    const client = { socket, session };
    browserSockets.add(client);
    const machineId = request.searchParams.get('node') || 'local';
    const pane = request.searchParams.get('pane');
    const lease = machineId + ':' + pane;
    let watch;
    let closeWatch;
    let sequence = 0;
    let reading = false;
    let queued = 0;
    let writes = Promise.resolve();

    const send = (packet) => {
      if (socket.readyState === WebSocket.OPEN) {
        if (socket.bufferedAmount > 512 * 1024) socket.close(1013, 'Terminal reader is too slow');
        else socket.send(JSON.stringify(packet));
      }
    };
    const report = (error) => send({ error: errorMessage(error, 'Terminal unavailable.') });
    const release = () => { if (leases.get(lease) === client) leases.delete(lease); };

    const loop = setInterval(async () => {
      if (!auth.valid(session)) { socket.close(4001, 'Session expired'); return; }
      if (!watch || reading || socket.readyState !== WebSocket.OPEN) return;
      reading = true;
      try {
        const result = await node(machineId).request('read', { watch, after: sequence });
        sequence = result.seq;
        if (result.initial || result.data) send(result);
      } catch (error) {
        report(error);
        socket.close(1011, 'Terminal connection lost');
      } finally { reading = false; }
    }, 120);

    socket.on('error', () => {});
    socket.on('close', () => {
      clearInterval(loop);
      browserSockets.delete(client);
      release();
      closeWatch?.();
    });
    socket.on('message', (data) => {
      if (!auth.session(req)) { socket.close(4001, 'Session expired'); return; }
      let packet;
      try { packet = JSON.parse(data); } catch { socket.close(1002, 'Invalid terminal message'); return; }
      if (!watch || !packet || typeof packet !== 'object') return;
      if (++queued > 32) {
        socket.close(1008, 'Too much terminal input');
        return;
      }
      writes = writes.then(async () => {
        if (socket.readyState !== WebSocket.OPEN || !auth.valid(session)) return;
        if (packet.type === 'take') {
          if (leases.has(lease) && leases.get(lease) !== client) throw new Failure(409, 'Another web client has terminal input.');
          leases.set(lease, client);
          send({ writable: true });
        } else if (packet.type === 'release') {
          release();
          send({ writable: false });
        } else if (packet.type === 'input' || packet.type === 'resize') {
          if (leases.get(lease) !== client) throw new Failure(403, 'Enable terminal input first.');
          await node(machineId).request(packet.type, { watch, data: packet.data, cols: packet.cols, rows: packet.rows });
        }
      }).catch(report).finally(() => { queued--; });
    });

    Promise.resolve().then(async () => {
      if (typeof pane !== 'string' || !/^\d+(?:\.\d+){3}$/.test(pane)) throw new Failure(400, 'Invalid Pane.');
      const target = node(machineId);
      const opened = await target.request('open', { pane });
      closeWatch = () => Promise.resolve(target.request('close', { watch: opened.watch })).catch(() => {});
      if (socket.readyState !== WebSocket.OPEN) { closeWatch(); return; }
      watch = opened.watch;
      sequence = opened.seq;
      send({ initial: opened.initial, rows: opened.rows, cols: opened.cols, resizable: opened.resizable });
    }).catch((error) => {
      report(error);
      socket.close(1011, 'Terminal unavailable');
    });
  }

  server.on('upgrade', (req, socket, head) => {
    socket.on('error', () => {});
    let request;
    try { request = new URL(req.url, url); } catch { socket.destroy(); return; }
    try {
      if (request.pathname === '/node/control' && !req.headers.origin) {
        const id = nodeName(req.headers['x-workbench-node']);
        const record = store.enrolled(id);
        const token = req.headers.authorization?.match(/^Bearer (.+)$/)?.[1];
        if (!record || !sameSecret(record.digest, token) || req.headers['x-workbench-protocol'] !== nodeProtocol) throw new Failure(401, 'Node authentication failed.');
        peers.handleUpgrade(req, socket, head, (ws) => {
          try { store.markConnected(record); } catch { ws.close(1011, 'Unable to record node connection'); return; }
          remote.get(id)?.socket.close(4000, 'Replaced by node reconnect');
          const peer = new RemoteNode(ws, () => { if (remote.get(id) === peer) remote.delete(id); });
          remote.set(id, peer);
          peer.heartbeat = true;
          ws.on('pong', () => { peer.heartbeat = true; });
        });
        return;
      }
      if (request.pathname === '/api/terminal') {
        auth.writeOrigin(req);
        const session = auth.session(req);
        if (!session || browserSockets.size >= 20) throw new Failure(401, 'Terminal unavailable.');
        browser.handleUpgrade(req, socket, head, (ws) => terminal(ws, req, request, session));
        return;
      }
    } catch {}
    socket.end('HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n');
  });

  const tick = setInterval(() => {
    auth.cleanup();
    local.cleanup();
    for (const [id, peer] of remote) {
      if (!store.enrolled(id) || !peer.heartbeat) peer.socket.terminate();
      else { peer.heartbeat = false; peer.socket.ping(); }
    }
  }, 10_000);

  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, '127.0.0.1', resolve);
  }).catch((error) => { clearInterval(tick); throw error; });
  return {
    address: server.address(),
    async close() {
      clearInterval(tick);
      for (const client of browserSockets) client.socket.terminate();
      for (const peer of remote.values()) peer.socket.terminate();
      peers.close();
      browser.close();
      local.close();
      await new Promise((resolve) => server.close(resolve));
    },
  };
}
