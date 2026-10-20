/** Durable declarative-plugin ownership. The native bridge serializes RPCs;
 * this service also serializes writes for direct callers. A candidate holds
 * mutations until atomic replacement succeeds, then publishes only durable
 * state into the existing host. Context and outstanding request ownership are
 * never replaced by an older copy after an await. */
import * as fs from 'node:fs/promises';
import { constants } from 'node:fs';
import { createHash, randomUUID } from 'node:crypto';
import path from 'node:path';
import { ExtensionHost } from './host.mjs';
import { readManifest } from './manifest.mjs';
import { readPackage, PACKAGE_FORMAT } from './package.mjs';
import { fail } from './json.mjs';

const STORE_LIMIT = 3 * 1024 * 1024;
const STORE_WARNING = 'Extension store unreadable; retained for recovery. No grants loaded.';
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const sameFile = (left, right) => left.dev === right.dev && left.ino === right.ino &&
  left.size === right.size && left.mtimeMs === right.mtimeMs && left.ctimeMs === right.ctimeMs;
const exact = (value, required, optional = []) => value && typeof value === 'object' && !Array.isArray(value) &&
  required.every(key => Object.hasOwn(value, key)) && Object.keys(value).every(key => [...required, ...optional].includes(key));

function parseExtension(source) {
  return JSON.parse(source).packageFormat === PACKAGE_FORMAT ? readPackage(source) : readManifest(source);
}

async function readStore(file) {
  let handle, entry;
  try {
    entry = await fs.lstat(file);
    if (!entry.isFile() || entry.isSymbolicLink()) throw new Error('Invalid extension store file');
    handle = await fs.open(file, constants.O_RDONLY | constants.O_NOFOLLOW);
  } catch (error) {
    if (error.code === 'ENOENT') return null;
    throw error;
  }
  try {
    const stat = await handle.stat();
    const current = await fs.lstat(file);
    if (!stat.isFile() || current.isSymbolicLink() || !current.isFile() ||
        !sameFile(entry, stat) || !sameFile(current, stat) || stat.size > STORE_LIMIT)
      throw new Error('Invalid or replaced extension store file');
    const bytes = Buffer.alloc(Math.min(STORE_LIMIT + 1, stat.size + 1));
    let offset = 0;
    while (offset < bytes.length) {
      const { bytesRead } = await handle.read(bytes, offset, bytes.length - offset, offset);
      if (!bytesRead) break;
      offset += bytesRead;
    }
    // Reading one byte beyond the original length detects growth without an
    // unbounded readFile allocation or accepting a silently truncated document.
    if (offset !== stat.size || offset > STORE_LIMIT || !sameFile(stat, await handle.stat()))
      throw new Error('Extension store changed while reading');
    return bytes.subarray(0, offset);
  } finally {
    await handle.close();
  }
}

/** Rename is the publication boundary. Errors before it preserve the previous
 * target; cleanup only removes this invocation's random temporary file. A
 * directory sync after publication is best effort: an already committed
 * replacement must never be reported as an unchanged failed transaction. */
async function atomicWrite(file, bytes, expectedDigest) {
  const temporary = path.join(path.dirname(file), `.extensions-${randomUUID()}.new`);
  let handle, committed = false;
  try {
    handle = await fs.open(temporary, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600);
    await handle.writeFile(bytes);
    await handle.sync();
    await handle.close(); handle = null;
    const previous = await readStore(file);
    if ((previous === null ? null : digest(previous)) !== expectedDigest)
      fail('STORE_CHANGED', 'Extension store changed outside this host; reload before making changes.');
    await fs.rename(temporary, file);
    committed = true;
    let directory;
    try {
      directory = await fs.open(path.dirname(file), constants.O_RDONLY | constants.O_DIRECTORY);
      await directory.sync();
    } catch {
      // The replacement is committed; some supported filesystems do not allow
      // syncing a directory handle. Do not roll back an acknowledged rename.
    } finally {
      await directory?.close().catch(() => {});
    }
  } finally {
    await handle?.close().catch(() => {});
    if (!committed) await fs.unlink(temporary).catch(() => {});
  }
}

function loadDocument(source, clock) {
  const saved = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(source));
  if (!exact(saved, ['schemaVersion', 'entries'], ['safeMode']) || saved.schemaVersion !== 1 ||
      !Array.isArray(saved.entries) || saved.entries.length > 32 ||
      (Object.hasOwn(saved, 'safeMode') && typeof saved.safeMode !== 'boolean'))
    throw new Error('Invalid extension store');
  const host = new ExtensionHost({ clock }), records = new Map();
  for (const record of saved.entries) {
    if (!exact(record, ['id', 'digest', 'source', 'grants', 'enabled'], ['settings']) || records.has(record.id) ||
        typeof record.source !== 'string' || !Array.isArray(record.grants) ||
        new Set(record.grants).size !== record.grants.length || typeof record.enabled !== 'boolean')
      throw new Error('Invalid stored extension');
    const loaded = parseExtension(record.source);
    if (loaded.manifest.id !== record.id || loaded.digest !== record.digest)
      throw new Error('Extension store content changed');
    const declared = loaded.manifest.capabilities.map(capability => capability.id);
    if (record.grants.some(capability => !declared.includes(capability))) throw new Error('Invalid stored grant');
    // Restoring previously approved data is host-owned. Revoked required
    // grants are restored as suspended rather than silently granted again.
    const review = JSON.parse(record.source).packageFormat === PACKAGE_FORMAT
      ? host.preparePackageReview(record.source) : host.prepareReview(record.source);
    host.confirmReview(review.token, declared);
    if (Object.hasOwn(record, 'settings')) host.restoreSettings(record.id, record.settings);
    for (const capability of declared) if (!record.grants.includes(capability)) host.revoke(record.id, capability);
    if (!record.enabled) host.disable(record.id);
    records.set(record.id, { ...record, grants: [...record.grants] });
  }
  host.setSafeMode(saved.safeMode ?? false);
  host.validateProjectionBudget();
  return { host, records };
}

export class ExtensionStore {
  #file;
  #host;
  #records = new Map();
  #sources = new Map();
  #warning = null;
  #diskDigest = null;
  #sequence = Promise.resolve();
  #writing = false;
  #clock;
  #write;

  constructor({ file, clock = Date.now, write = atomicWrite }) {
    if (typeof file !== 'string' || !path.isAbsolute(file)) throw new Error('Extension store needs an absolute file path');
    this.#file = file;
    this.#clock = clock;
    this.#write = write;
    this.#host = new ExtensionHost({ clock });
  }

  static async open(options) {
    const store = new ExtensionStore(options);
    try {
      const bytes = await readStore(store.#file);
      if (bytes !== null) {
        const { host, records } = loadDocument(bytes, store.#clock);
        store.#host = host;
        store.#records = records;
        store.#diskDigest = digest(bytes);
      }
    } catch {
      // Malformed/future/linked data stays available for manual recovery. No
      // default document is written over it, even after toggling safe mode.
      store.#warning = STORE_WARNING;
      store.#host.setSafeMode(true);
    }
    return store;
  }

  get warning() { return this.#warning; }
  get safeMode() { return this.#host.safeMode; }
  get pendingReviewCount() { this.#pruneSources(); return this.#sources.size; }
  list() { return this.#host.list(); }
  inspect(id) { return this.#host.inspect(id); }

  #available() {
    if (this.#warning) fail('STORE_UNREADABLE', this.#warning);
  }
  #requestAvailable() {
    this.#available();
    if (this.#writing) fail('STORE_BUSY', 'Extension state is being saved; retry after the operation finishes.');
  }
  #pruneSources() {
    for (const [token, value] of this.#sources)
      if (value.expiresAt <= this.#clock() || !this.#host.hasReview(token)) {
        this.#sources.delete(token); this.#host.cancelReview(token);
      }
  }
  #review(source, packaged) {
    this.#requestAvailable();
    this.#pruneSources();
    const review = packaged ? this.#host.preparePackageReview(source) : this.#host.prepareReview(source);
    this.#sources.set(review.token, { source, expiresAt: review.expiresAt });
    return review;
  }
  prepareReview(source) { return this.#review(source, false); }
  preparePackageReview(source) { return this.#review(source, true); }
  cancelReview(token) { this.#sources.delete(token); return this.#host.cancelReview(token); }
  rereview(id) {
    this.#requestAvailable();
    const record = this.#records.get(id);
    if (!record) fail('HOST_NOT_FOUND', 'Extension is not installed in this host session.');
    return this.#review(record.source, JSON.parse(record.source).packageFormat === PACKAGE_FORMAT);
  }

  #expected(id, expected) {
    const current = this.#host.inspect(id);
    if (expected !== undefined && (!exact(expected, ['digest', 'generation']) ||
        expected.digest !== current.digest || expected.generation !== current.generation))
      fail('HOST_STALE_ENTRY', 'Extension state changed; refresh the current entry before trying again.');
  }

  #mutate(operation) {
    const pending = this.#sequence.catch(() => {}).then(async () => {
      this.#available();
      this.#writing = true;
      try {
        const candidate = this.#host.fork(), sources = new Map(this.#records);
        const result = operation(candidate, sources);
        candidate.validateProjectionBudget();
        const entries = candidate.list().map(entry => {
          const record = sources.get(entry.id);
          if (!record) throw new Error('Installed extension source is unavailable');
          const settings = candidate.settingsValues(entry.id);
          return { id: entry.id, digest: entry.digest, source: record.source, grants: [...entry.grants], enabled: entry.enabled,
            ...(Object.keys(settings).length ? { settings } : {}) };
        });
        const bytes = Buffer.from(JSON.stringify({ schemaVersion: 1, entries, safeMode: candidate.safeMode }));
        if (bytes.length > STORE_LIMIT) fail('STORE_SIZE', 'Extension store exceeds the supported size; previous state retained.');
        await this.#write(this.#file, bytes, this.#diskDigest);
        this.#diskDigest = digest(bytes);
        this.#records = new Map(entries.map(record => [record.id, record]));
        this.#host.publishState(candidate);
        this.#pruneSources();
        return result;
      } finally {
        this.#writing = false;
      }
    });
    this.#sequence = pending;
    return pending;
  }

  confirmReview(token, grants) {
    return this.#mutate((candidate, sources) => {
      const review = this.#sources.get(token);
      // Consent is single use even if validation or disk persistence fails.
      this.#sources.delete(token); this.#host.cancelReview(token);
      if (!review) fail('HOST_TOKEN', 'Extension review expired, was cancelled, or has already been used.');
      const result = candidate.confirmReview(token, grants);
      sources.set(result.id, { source: review.source });
      return result;
    });
  }
  enable(id, expected) {
    return this.#mutate(candidate => { this.#expected(id, expected); return candidate.enable(id); });
  }
  disable(id, expected) {
    return this.#mutate(candidate => { this.#expected(id, expected); return candidate.disable(id); });
  }
  revoke(id, capability, expected) {
    return this.#mutate(candidate => { this.#expected(id, expected); return candidate.revoke(id, capability); });
  }
  remove(id, expected) {
    return this.#mutate((candidate, sources) => {
      this.#expected(id, expected);
      const result = candidate.remove(id);
      sources.delete(id);
      return result;
    });
  }
  setSafeMode(enabled) { return this.#mutate(candidate => candidate.setSafeMode(enabled)); }
  updateSettings(id, values, expected) {
    return this.#mutate(candidate => {
      if (expected === undefined) fail('HOST_STALE_ENTRY', 'Plugin settings require the displayed entry snapshot.');
      this.#expected(id, expected);
      return candidate.updateSettings(id, values);
    });
  }

  setContext(context) { return this.#host.setContext(context); }
  cards(slot) { return this.#host.cards(slot); }
  prepareAction(extensionId, cardId, actionId, expected) {
    this.#requestAvailable(); this.#expected(extensionId, expected);
    return this.#host.prepareAction(extensionId, cardId, actionId);
  }
  cancelAction(token) { return this.#host.cancelAction(token); }
  commitAction(token) { this.#requestAvailable(); return this.#host.commitAction(token); }
}
