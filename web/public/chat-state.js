const draftPrefix = 'workbench-chat-v1/draft/';
const deliveryPrefix = 'workbench-chat-v1/delivery/';
const settingsPrefix = 'workbench-chat-v1/settings/';

function normalizeText(text) {
  return text.replace(/\r\n?/g, '\n').trim();
}

function sameImages(left = [], right = []) {
  return left.length === right.length && left.every((image, index) => image.content === right[index].content);
}

export class ChatState {
  #storage;
  #drafts = new Map();
  #deliveries = new Map();
  #pendingSettings = new Map();
  #models = new Map();
  #permissions = new Map();

  constructor(storage) { this.#storage = storage; }

  #persist(key, value) {
    try {
      if (value) this.#storage().setItem(key, JSON.stringify(value));
      else this.#storage().removeItem(key);
    } catch {}
  }

  clear() {
    this.#drafts.clear();
    this.#deliveries.clear();
    this.#pendingSettings.clear();
    this.#models.clear();
    this.#permissions.clear();
  }

  restore(unconfirmedMessage) {
    this.clear();
    try {
      const storage = this.#storage();
      for (const key of Object.keys(storage)) {
        if (![draftPrefix, deliveryPrefix, settingsPrefix].some((prefix) => key.startsWith(prefix))) continue;
        let value;
        try { value = JSON.parse(storage.getItem(key)); } catch { continue; }
        if (key.startsWith(settingsPrefix)) {
          if (value?.model || value?.permission) this.#pendingSettings.set(key.slice(settingsPrefix.length), value);
          continue;
        }
        if (typeof value?.text !== 'string' || (!value.text && !value.images?.length)) continue;
        if (key.startsWith(draftPrefix)) {
          this.#drafts.set(key.slice(draftPrefix.length), value);
          continue;
        }
        if (key !== deliveryPrefix + value.id || typeof value.key !== 'string'
            || !['sending', 'sent', 'failed'].includes(value.state)) continue;
        if (value.state === 'sending') {
          value.state = 'failed';
          value.error = unconfirmedMessage;
        }
        this.#queue(value.key).entries.push(value);
      }
      for (const [key, queue] of this.#deliveries) {
        queue.entries.sort((a, b) => a.startedAt - b.startedAt);
        const failed = queue.entries.findLast((entry) => entry.state === 'failed');
        if (failed && !this.#drafts.has(key)) this.saveDraft(key, failed.text, failed.id, failed.images);
      }
    } catch {}
  }

  draft(key) { return this.#drafts.get(key)?.text || ''; }
  draftImages(key) { return this.#drafts.get(key)?.images || []; }

  saveDraft(key, text, retry, images = this.draftImages(key)) {
    if (text || images.length) this.#drafts.set(key, { text, retry, ...(images.length && { images }) });
    else this.#drafts.delete(key);
    this.#persist(draftPrefix + key, this.#drafts.get(key));
  }

  clearDraft(key) { this.saveDraft(key, '', undefined, []); }

  settings(key) {
    return { pending: this.#pendingSettings.get(key), model: this.#models.get(key), permission: this.#permissions.get(key) };
  }

  observeModel(key, model) { this.#models.set(key, model); }

  observeSettings(key, { model, effort, label, models, permission }) {
    if (model) this.observeModel(key, { model, effort, label: label || models?.find((item) => item.id === model)?.label || model });
    if (permission?.mode) this.#permissions.set(key, permission);
  }

  #setPending(key, settings) {
    if (settings.model || settings.permission) this.#pendingSettings.set(key, settings);
    else this.#pendingSettings.delete(key);
    this.#persist(settingsPrefix + key, this.#pendingSettings.get(key));
  }

  queueSettings(key, changes) {
    this.#setPending(key, { ...this.#pendingSettings.get(key), ...changes });
  }

  applySettings(key, { model, effort, label, permission }) {
    const pending = { ...this.#pendingSettings.get(key) };
    if (model) {
      this.observeModel(key, { model, effort, label });
      delete pending.model;
      delete pending.effort;
      delete pending.label;
    }
    if (permission) delete pending.permission;
    this.#setPending(key, pending);
  }

  promote(previous, key) {
    if (previous === key) return;
    for (const records of [this.#models, this.#permissions]) {
      if (!records.has(previous)) continue;
      records.set(key, records.get(previous));
      records.delete(previous);
    }
    const pending = this.#pendingSettings.get(previous);
    if (pending) {
      const existing = this.#pendingSettings.get(key);
      this.#setPending(key, existing ? { ...pending, ...existing } : pending);
      this.#setPending(previous, {});
    }
    const draft = this.#drafts.get(previous);
    if (draft) {
      if (!this.#drafts.has(key)) this.saveDraft(key, draft.text, draft.retry, draft.images);
      this.clearDraft(previous);
    }
    const queue = this.#deliveries.get(previous);
    if (queue) {
      for (const entry of queue.entries) {
        entry.key = key;
        this.#persist(deliveryPrefix + entry.id, entry);
      }
      const current = this.#deliveries.get(key);
      if (current) {
        current.entries.push(...queue.entries);
        current.entries.sort((a, b) => a.startedAt - b.startedAt);
        for (const id of queue.matched) current.matched.add(id);
      } else this.#deliveries.set(key, queue);
      this.#deliveries.delete(previous);
    }
  }

  #queue(key) {
    let queue = this.#deliveries.get(key);
    if (!queue) {
      queue = { entries: [], matched: new Set() };
      this.#deliveries.set(key, queue);
    }
    return queue;
  }

  deliveries(key) { return this.#deliveries.get(key)?.entries || []; }

  failedDelivery(key, text, images) {
    return this.deliveries(key).findLast((entry) => entry.state === 'failed' && entry.text === text && sameImages(entry.images, images));
  }

  beginDelivery(key, { text, images = [], previous, after, replaceDraft }) {
    const entries = this.deliveries(key);
    if (previous && (previous.state !== 'failed' || !entries.includes(previous))) return null;
    const entry = previous || { id: crypto.randomUUID(), key, text, match: normalizeText(text), ...(images.length && { images }) };
    entry.state = 'sending';
    entry.startedAt = Date.now();
    entry.after = after;
    if (previous) entries.splice(entries.indexOf(previous), 1);
    this.#queue(key).entries.push(entry);
    this.#persist(deliveryPrefix + entry.id, entry);
    const clearDraft = replaceDraft || this.#drafts.get(key)?.retry === entry.id;
    if (clearDraft) this.clearDraft(key);
    return { entry, clearDraft };
  }

  sent(entry, { after, submittedAt }) {
    if (!this.deliveries(entry.key).includes(entry)) return;
    entry.state = 'sent';
    if (Number.isSafeInteger(after) && after >= 0) entry.after = after;
    if (Number.isFinite(submittedAt)) entry.startedAt = submittedAt;
    this.#persist(deliveryPrefix + entry.id, entry);
  }

  failed(entry, message) {
    if (!this.deliveries(entry.key).includes(entry)) return false;
    entry.state = 'failed';
    entry.error = message;
    this.#persist(deliveryPrefix + entry.id, entry);
    if (!this.#drafts.has(entry.key)) this.saveDraft(entry.key, entry.text, entry.id, entry.images ?? []);
    return true;
  }

  reconcile(key, messages) {
    const queue = this.#deliveries.get(key);
    const result = { changed: false, draftCleared: false };
    if (!queue?.entries.length) return result;
    for (const message of messages) {
      if (message.role !== 'user' || queue.matched.has(message.id)) continue;
      const text = normalizeText(message.text);
      const index = queue.entries.findIndex((entry) => text === entry.match && (entry.images?.length || 0) === (message.images || 0) && (entry.after !== null
        ? message.id >= entry.after : entry.state !== 'sending' && Date.parse(message.time) >= entry.startedAt));
      if (index < 0) continue;
      const [entry] = queue.entries.splice(index, 1);
      this.#persist(deliveryPrefix + entry.id, null);
      if (this.#drafts.get(key)?.retry === entry.id) {
        this.clearDraft(key);
        result.draftCleared = true;
      }
      queue.matched.add(message.id);
      result.changed = true;
    }
    if (!queue.entries.length) this.#deliveries.delete(key);
    return result;
  }
}
