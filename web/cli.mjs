import { hostname } from 'node:os';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import { Writable } from 'node:stream';
import { parseArgs } from 'node:util';
import { defaultState, Failure, nodeName, parseOrigin, readJson, Store, writeJson } from './state.mjs';
import { LocalNode } from './tmux.mjs';

const usage = 'Usage: tmux-agent-workbench web COMMAND\n\n'
  + '  serve [--origin URL] [--listen 127.0.0.1:PORT] [--name NAME] [--socket PATH]\n'
  + '        [--cert PATH --key PATH] [--fabric-node NAME=EXECUTOR] [--observe-snapshot]\n'
  + '  invite --origin URL              Create a one-time Passkey enrollment link\n'
  + '  node invite NAME                Create a one-time node invitation\n'
  + '  node join --gateway URL         Enroll this host (invitation read from stdin)\n'
  + '  node run [--socket PATH] [--observe-snapshot]   Run the enrolled host connector\n'
  + '  node list | node revoke NAME    Inspect or revoke paired nodes\n'
  + '  node forget                     Remove this host’s saved connector credential\n\n'
  + 'Commands accept --state PATH to override the XDG state directory.\n'
  + 'Set WORKBENCH_WEB_TERMINAL_THEME to a JSON file with light and dark terminal palettes.\n';

function configuration(args) {
  return parseArgs({
    args,
    allowPositionals: true,
    options: {
      origin: { type: 'string' }, listen: { type: 'string' }, name: { type: 'string' },
      socket: { type: 'string' }, cert: { type: 'string' }, key: { type: 'string' },
      state: { type: 'string' }, gateway: { type: 'string' },
      'fabric-node': { type: 'string', multiple: true },
      'observe-snapshot': { type: 'boolean' },
    },
  });
}

async function inputSecret() {
  if (!process.stdin.isTTY) {
    // A pipe from SSH can be temporarily empty and nonblocking on macOS.
    const reader = createInterface({ input: process.stdin, crlfDelay: Infinity });
    for await (const line of reader) {
      reader.close();
      return line.trim();
    }
    throw new Error('No invitation was provided.');
  }
  process.stderr.write('Node invitation: ');
  const output = new Writable({ write(_chunk, _encoding, callback) { callback(); } });
  const reader = createInterface({ input: process.stdin, output, terminal: true });
  return new Promise((resolve, reject) => {
    reader.once('line', (line) => { resolve(line.trim()); reader.close(); });
    reader.once('SIGINT', () => reader.close());
    reader.once('close', () => { process.stderr.write('\n'); reject(new Error('Pairing canceled.')); });
  });
}

function local(values) {
  return new LocalNode({
    socket: values.socket,
    core: process.env.WORKBENCH_WEB_CORE,
    usage: process.env.WORKBENCH_WEB_USAGE,
    observeSnapshot: values['observe-snapshot'],
  });
}

function stopSignal() {
  const controller = new AbortController();
  for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, () => controller.abort());
  return controller.signal;
}

async function main() {
  const [verb, ...args] = process.argv.slice(2);
  if (!verb || verb === 'help' || verb === '--help' || args.includes('--help')) {
    process.stdout.write(usage);
    return;
  }
  const { values, positionals } = configuration(args);
  const state = values.state || defaultState;
  const store = new Store(state);
  const connector = join(state, 'connector.json');

  if (verb === 'serve' && positionals.length === 0) {
    const { startServer } = await import('./server.mjs');
    const fabric = (values['fabric-node'] || []).map((entry) => {
      const [name, executor, ...rest] = entry.split('=');
      if (!executor || rest.length) throw new Failure(400, 'Use --fabric-node NAME=EXECUTOR.');
      return [nodeName(name), executor];
    });
    const origin = values.origin || 'http://localhost:7080';
    const signal = stopSignal();
    const service = await startServer({
      local: local(values), localName: values.name || hostname(), store,
      listen: values.listen || '127.0.0.1:7080', origin, cert: values.cert, key: values.key, fabric,
      terminalTheme: process.env.WORKBENCH_WEB_TERMINAL_THEME,
    });
    process.stderr.write('Workbench Web listening on ' + service.address.address + ':' + service.address.port + ' for ' + origin + '\n');
    if (!signal.aborted) await new Promise((resolve) => signal.addEventListener('abort', resolve, { once: true }));
    await service.close();
    return;
  }

  if (verb === 'invite' && positionals.length === 0) {
    const origin = parseOrigin(values.origin || 'http://localhost:7080').origin;
    process.stdout.write(store.issuePasskeyLink(origin).url + '\n');
    return;
  }

  if (verb === 'node') {
    const [action, name, ...extra] = positionals;
    if (extra.length) throw new Error(usage);
    if (action === 'invite' && name) {
      if (store.enrolled(name)) throw new Failure(409, 'Revoke the existing node before pairing again.');
      process.stdout.write(store.issue('node', nodeName(name)) + '\n');
      return;
    }
    if (action === 'list' && !name) {
      process.stdout.write(JSON.stringify(store.listNodes().map(({ id, enrolled }) => ({ id, enrolled })), null, 2) + '\n');
      return;
    }
    if (action === 'revoke' && name) {
      if (!store.revokeNode(name)) throw new Failure(404, 'Paired node not found.');
      process.stdout.write('Revoked ' + name + '\n');
      return;
    }
    if (action === 'forget' && !name) {
      if (!store.forgetConnector()) throw new Failure(404, 'No local connector credential.');
      process.stdout.write('Local connector credential removed.\n');
      return;
    }
    if (action === 'join' && !name) {
      if (readJson(connector)) throw new Failure(409, 'This state directory already has a paired connector.');
      if (!values.gateway) throw new Failure(400, 'Provide --gateway URL.');
      const { enrollNode } = await import('./connector.mjs');
      const registered = await enrollNode(values.gateway, await inputSecret());
      writeJson(connector, registered);
      process.stdout.write('Paired ' + registered.id + '. Start it with tmux-agent-workbench web node run.\n');
      return;
    }
    if (action === 'run' && !name) {
      const config = readJson(connector);
      if (!config) throw new Failure(404, 'Pair this machine with web node join first.');
      const { runConnector } = await import('./connector.mjs');
      await runConnector(config, local(values), stopSignal());
      return;
    }
  }
  throw new Error(usage);
}

main().catch((error) => {
  const message = error.code === 'ERR_MODULE_NOT_FOUND'
    ? 'Run npm ci in the tmux-agent-workbench web directory to install the optional Web dependencies.'
    : error.message;
  process.stderr.write('Workbench Web: ' + message + '\n');
  process.exitCode = 1;
});
