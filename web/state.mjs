import { createHash, randomBytes } from 'node:crypto';
import { mkdirSync, readFileSync, readdirSync, renameSync, statSync, unlinkSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

export const stateHome = process.env.XDG_STATE_HOME || join(homedir(), '.local', 'state');
export const workbenchState = join(stateHome, 'tmux-agent-workbench');
export const defaultState = join(workbenchState, 'web');
export const secret = () => randomBytes(32).toString('base64url');
export const hash = (value) => createHash('sha256').update(value).digest('hex');

export class Failure extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
}

export function readJson(path, fallback = null) {
  try {
    return JSON.parse(readFileSync(path, 'utf8'));
  } catch (error) {
    if (error.code === 'ENOENT') return fallback;
    throw error;
  }
}

export function writeJson(path, contents) {
  const temporary = path + '.' + secret();
  writeFileSync(temporary, JSON.stringify(contents, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
  try {
    renameSync(temporary, path);
  } finally {
    remove(temporary);
  }
}

function remove(path) {
  try {
    unlinkSync(path);
    return true;
  } catch (error) {
    if (error.code === 'ENOENT') return false;
    throw error;
  }
}

export function parseOrigin(value) {
  let url;
  try { url = new URL(value); } catch { throw new Failure(400, 'Provide a valid gateway origin.'); }
  const localhost = ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname);
  if (
    (url.protocol !== 'https:' && !(url.protocol === 'http:' && localhost))
    || url.username || url.password || url.pathname !== '/' || url.search || url.hash
  ) throw new Failure(400, 'Use an HTTPS origin or a loopback HTTP origin for local testing.');
  return url;
}

const nodeIdPattern = /^[A-Za-z0-9][A-Za-z0-9._-]{0,62}$/;
export function nodeName(value) {
  if (typeof value !== 'string' || !nodeIdPattern.test(value) || ['local', 'all'].includes(value)) {
    throw new Failure(400, 'Invalid node name. Use letters, digits, dots, underscores or hyphens.');
  }
  return value;
}

export class Store {
  constructor(directory) {
    this.directory = directory;
    this.invites = join(directory, 'invites');
    this.nodes = join(directory, 'nodes');
    this.credentials = join(directory, 'passkeys.json');
    for (const path of [directory, this.invites, this.nodes]) {
      mkdirSync(path, { recursive: true, mode: 0o700 });
      const owner = statSync(path);
      if (owner.uid !== process.getuid() || (owner.mode & 0o077)) {
        throw new Failure(400, 'Web state directories must be private and owned by the Workbench user.');
      }
    }
  }

  issue(kind, node = null) {
    const token = secret();
    // Keep a bounded inventory when an operator repeatedly regenerates invitations.
    for (const file of readdirSync(this.invites)) {
      if (!/^[0-9a-f]{64}\.json$/.test(file)) continue;
      const path = join(this.invites, file);
      if ((readJson(path)?.expires ?? 0) < Date.now()) remove(path);
    }
    writeJson(join(this.invites, hash(token) + '.json'), { kind, node, expires: Date.now() + 10 * 60_000 });
    return token;
  }

  invitation(token, kind, consume = false) {
    if (typeof token !== 'string' || !/^[A-Za-z0-9_-]{43}$/.test(token)) {
      throw new Failure(403, 'Invitation is invalid or expired.');
    }
    const path = join(this.invites, hash(token) + '.json');
    const invite = readJson(path);
    if (invite?.kind !== kind || invite.expires <= Date.now() || (consume && !remove(path))) {
      throw new Failure(403, 'Invitation is invalid or expired.');
    }
    return invite;
  }

  enrolled(name) {
    return readJson(join(this.nodes, nodeName(name) + '.json'));
  }

  enroll(token) {
    const invite = this.invitation(token, 'node');
    const name = nodeName(invite.node);
    if (this.enrolled(name)) throw new Failure(409, 'Revoke the existing node before pairing again.');
    const credential = secret();
    this.invitation(token, 'node', true);
    writeJson(join(this.nodes, name + '.json'), { id: name, digest: hash(credential), enrolled: Date.now() });
    return { id: name, credential };
  }

  markConnected(record) {
    if (record.connected) return;
    writeJson(join(this.nodes, record.id + '.json'), { ...record, connected: Date.now() });
  }

  forgetConnector() {
    return remove(join(this.directory, 'connector.json'));
  }

  pendingNodes() {
    return new Set(readdirSync(this.invites).filter((file) => /^[0-9a-f]{64}\.json$/.test(file))
      .map((file) => readJson(join(this.invites, file)))
      .filter((invite) => invite?.kind === 'node' && invite.expires > Date.now())
      .map(({ node }) => node));
  }

  listNodes() {
    return readdirSync(this.nodes)
      .filter((file) => file.endsWith('.json'))
      .map((file) => readJson(join(this.nodes, file)))
      .filter((record) => record?.id && record.digest);
  }

  revokeNode(name) {
    return remove(join(this.nodes, nodeName(name) + '.json'));
  }

  keys() { return readJson(this.credentials, []); }
  writeKeys(keys) { writeJson(this.credentials, keys); }
}
