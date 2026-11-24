import {
  generateAuthenticationOptions,
  generateRegistrationOptions,
  verifyAuthenticationResponse,
  verifyRegistrationResponse,
} from '@simplewebauthn/server';
import { timingSafeEqual } from 'node:crypto';
import { Failure, hash, secret } from './state.mjs';

const CHALLENGE_AGE = 2 * 60_000;
const SESSION_IDLE = 30 * 60_000;
const SESSION_AGE = 12 * 60 * 60_000;

export class Auth {
  pending = new Map();
  sessions = new Map();
  limits = new Map();

  constructor(store, origin) {
    this.store = store;
    this.origin = origin.origin;
    this.rpID = origin.hostname.startsWith('[') ? 'localhost' : origin.hostname;
    this.secure = origin.protocol === 'https:';
    this.cookie = this.secure ? '__Host-workbench' : 'workbench-local';
  }

  writeOrigin(request) {
    if (request.headers.origin !== this.origin) throw new Failure(403, 'Unexpected request origin.');
  }

  throttle(request) {
    const ip = request.socket.remoteAddress;
    for (const [key, maximum] of [[ip, 35], ['all', 180]]) {
      const now = Date.now();
      const limit = this.limits.get(key);
      if (!limit || limit.until <= now) this.limits.set(key, { count: 1, until: now + 60_000 });
      else if (++limit.count > maximum) throw new Failure(429, 'Too many authentication attempts. Try again in a minute.');
    }
  }

  cleanup() {
    const now = Date.now();
    for (const [key, record] of this.pending) if (record.until <= now) this.pending.delete(key);
    for (const [key, session] of this.sessions) if (session.until <= now || session.idle <= now) this.sessions.delete(key);
    for (const [key, limit] of this.limits) if (limit.until <= now) this.limits.delete(key);
  }

  session(request, touch = true) {
    const token = request.headers.cookie?.split(/;\s*/).find((value) => value.startsWith(this.cookie + '='))?.slice(this.cookie.length + 1);
    const key = token ? hash(token) : null;
    const session = key ? this.sessions.get(key) : null;
    if (!session || !this.valid(key)) return null;
    if (touch) session.idle = Math.min(Date.now() + SESSION_IDLE, session.until);
    return key;
  }

  valid(key) {
    const session = this.sessions.get(key);
    return Boolean(session && session.until > Date.now() && session.idle > Date.now());
  }

  cookieHeader(value, expires = false) {
    return this.cookie + '=' + value + '; Path=/; HttpOnly; SameSite=Strict'
      + (this.secure ? '; Secure' : '') + '; Max-Age=' + (expires ? '0' : SESSION_AGE / 1000);
  }

  issueSession(credentialId) {
    const token = secret();
    const now = Date.now();
    this.sessions.set(hash(token), { credentialId, idle: now + SESSION_IDLE, until: now + SESSION_AGE });
    return this.cookieHeader(token);
  }

  challenge(kind, options, extra = {}) {
    this.cleanup();
    const ticket = secret();
    this.pending.set(hash(ticket), { kind, challenge: options.challenge, until: Date.now() + CHALLENGE_AGE, ...extra });
    return { ticket, options };
  }

  take(ticket, kind) {
    if (typeof ticket !== 'string' || ticket.length !== 43) throw new Failure(400, 'Invalid authentication challenge.');
    const key = hash(ticket);
    const record = this.pending.get(key);
    this.pending.delete(key);
    if (record?.kind !== kind || record.until <= Date.now()) {
      throw new Failure(400, 'Authentication challenge has expired.');
    }
    return record;
  }

  async registrationOptions(request, { invite, label }) {
    const session = this.session(request);
    if (!session) this.store.invitation(invite, 'passkey');
    const excludeCredentials = this.store.keys().map(({ id, transports }) => ({ id, transports }));
    const options = await generateRegistrationOptions({
      rpName: 'Workbench Web',
      rpID: this.rpID,
      userName: 'workbench-owner',
      userID: Buffer.from(hash(this.rpID + ':workbench-owner'), 'hex'),
      attestationType: 'none',
      authenticatorSelection: { residentKey: 'required', userVerification: 'required' },
      excludeCredentials,
    });
    return this.challenge('register', options, {
      invite: session ? null : invite,
      session,
      label: typeof label === 'string' && label.trim() ? label.trim().slice(0, 64) : 'Passkey',
    });
  }

  async register(request, { ticket, response }) {
    const pending = this.take(ticket, 'register');
    if (pending.session && pending.session !== this.session(request)) throw new Failure(401, 'Sign in again.');
    let verification;
    try {
      verification = await verifyRegistrationResponse({
        response,
        expectedChallenge: pending.challenge,
        expectedOrigin: this.origin,
        expectedRPID: this.rpID,
        requireUserVerification: true,
      });
    } catch {
      throw new Failure(403, 'Passkey registration failed.');
    }
    if (!verification.verified) throw new Failure(403, 'Passkey registration failed.');
    const { credential } = verification.registrationInfo;
    const keys = this.store.keys();
    if (keys.some(({ id }) => id === credential.id)) throw new Failure(409, 'This passkey is already registered.');
    if (pending.invite) this.store.invitation(pending.invite, 'passkey', true);
    else if (!this.valid(pending.session)) throw new Failure(401, 'Sign in again.');
    keys.push({
      id: credential.id, publicKey: Buffer.from(credential.publicKey).toString('base64url'),
      counter: credential.counter, transports: credential.transports,
      label: pending.label, created: Date.now(),
    });
    this.store.writeKeys(keys);
    return pending.session ? null : this.issueSession(credential.id);
  }

  async authenticationOptions() {
    const options = await generateAuthenticationOptions({ rpID: this.rpID, userVerification: 'required' });
    return this.challenge('login', options);
  }

  async login({ ticket, response }) {
    const pending = this.take(ticket, 'login');
    const stored = this.store.keys().find((key) => key.id === response?.id);
    if (!stored) throw new Failure(403, 'Passkey verification failed.');
    let verification;
    try {
      verification = await verifyAuthenticationResponse({
        response,
        expectedChallenge: pending.challenge,
        expectedOrigin: this.origin,
        expectedRPID: this.rpID,
        requireUserVerification: true,
        credential: { ...stored, publicKey: new Uint8Array(Buffer.from(stored.publicKey, 'base64url')) },
      });
    } catch {
      throw new Failure(403, 'Passkey verification failed.');
    }
    const keys = this.store.keys();
    const current = keys.find((key) => key.id === stored.id);
    if (!verification.verified || !current || current.counter !== stored.counter) {
      throw new Failure(403, 'Passkey verification failed.');
    }
    current.counter = verification.authenticationInfo.newCounter;
    this.store.writeKeys(keys);
    return this.issueSession(current.id);
  }

  passkeys() {
    return this.store.keys().map(({ id, label, created }) => ({ id, label, created }));
  }

  revoke(id) {
    const keys = this.store.keys();
    const kept = keys.filter((key) => key.id !== id);
    if (!kept.length) throw new Failure(409, 'Add a backup passkey before removing the last one.');
    if (kept.length === keys.length) throw new Failure(404, 'Passkey not found.');
    this.store.writeKeys(kept);
    for (const [key, session] of this.sessions) if (session.credentialId === id) this.sessions.delete(key);
  }
}

export function sameSecret(digest, token) {
  if (typeof token !== 'string' || !/^[A-Za-z0-9_-]{43}$/.test(token)) return false;
  return timingSafeEqual(Buffer.from(digest, 'hex'), Buffer.from(hash(token), 'hex'));
}
