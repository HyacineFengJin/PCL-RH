import { randomUUID, createHash } from 'node:crypto';
import { readManifest, actionCapability, compareVersions, extensionScope, CARD_SLOTS } from './manifest.mjs';
import { fail, freeze } from './json.mjs';
import { readPackage } from './package.mjs';
import { defaultSettings, retainSettings, validateSettingsPatch, resolveAppearance } from './settings.mjs';

const REVIEW_TTL = 5 * 60_000;
const ACTION_TTL = 30_000;
const MAX_PENDING = 64;
const MAX_INSTALLED = 32;
const SNAPSHOT = /^[A-Za-z0-9._+ -]{1,80}(?![\s\S])/;

/** Layout identifiers cannot fit both maximum-length external IDs verbatim.
 * Hash a length-unambiguous pair; settings/version changes retain the panel. */
export const panelViewId = (extensionId, cardId) => 'rh.extensions.panel.' +
  createHash('sha256').update(JSON.stringify([extensionId, cardId])).digest('hex');

// This object belongs to trusted host code. Extensions supply only JSON text.
// It is a reference policy implementation, not a JavaScript execution sandbox.
export class ExtensionHost {
  #entries = new Map();
  #reviews = new Map();
  #actions = new Map();
  #context = null;
  #contextGeneration = 0;
  #generation = 0;
  #safeMode = false;
  #consentEpoch = 0;
  #clock;

  constructor({ clock = Date.now } = {}) { this.#clock = clock; }

  /** Trusted persistence uses an isolated candidate. Ephemeral request/context
   * ownership stays with the published host while its bytes are being saved. */
  fork() {
    const copy = new ExtensionHost({ clock: this.#clock });
    copy.#entries = new Map([...this.#entries].map(([id, entry]) => [id, { ...entry, grants: new Set(entry.grants), settings: { ...entry.settings } }]));
    copy.#reviews = new Map(this.#reviews);
    copy.#actions = new Map(this.#actions);
    copy.#context = this.#context;
    copy.#contextGeneration = this.#contextGeneration;
    copy.#generation = this.#generation;
    copy.#safeMode = this.#safeMode;
    copy.#consentEpoch = this.#consentEpoch;
    return copy;
  }

  publishState(candidate) {
    if (!(candidate instanceof ExtensionHost)) fail('HOST_STATE', 'Expected a trusted host candidate.');
    const changed = new Set([...this.#entries.keys(), ...candidate.#entries.keys()].filter(id =>
      this.#entries.get(id)?.generation !== candidate.#entries.get(id)?.generation));
    this.#entries = candidate.#entries;
    this.#generation = candidate.#generation;
    this.#safeMode = candidate.#safeMode;
    if (this.#consentEpoch !== candidate.#consentEpoch) {
      this.#reviews.clear(); this.#actions.clear();
    } else {
      for (const [token, review] of this.#reviews)
        if (changed.has(review.loaded.manifest.id)) this.#reviews.delete(token);
      for (const [token, action] of this.#actions)
        if (changed.has(action.extensionId)) this.#actions.delete(token);
    }
    this.#consentEpoch = candidate.#consentEpoch;
  }

  get safeMode() { return this.#safeMode; }
  hasReview(token) { this.#clean(this.#reviews); return this.#reviews.has(token); }

  #clean(map) {
    for (const [token, value] of map) if (value.expiresAt <= this.#clock()) map.delete(token);
  }

  #store(map, value, ttl) {
    this.#clean(map);
    if (map.size >= MAX_PENDING) fail('HOST_PENDING_LIMIT', 'Too many pending requests; finish or cancel a review first.');
    const token = randomUUID();
    map.set(token, { ...value, expiresAt: this.#clock() + ttl });
    return token;
  }

  #take(map, token) {
    const value = map.get(token);
    map.delete(token);
    if (!value || value.expiresAt <= this.#clock()) fail('HOST_TOKEN', 'This request expired, was cancelled, or has already been used.');
    return value;
  }

  prepareReview(source) {
    return this.#prepare(readManifest(source));
  }

  preparePackageReview(source) {
    return this.#prepare(readPackage(source));
  }

  #prepare(loaded) {
    const { manifest, digest } = loaded;
    const previous = this.#entries.get(manifest.id);
    if (!previous && this.#entries.size >= MAX_INSTALLED) fail('HOST_INSTALL_LIMIT', 'Experimental host extension limit reached.');
    if (previous) {
      const order = compareVersions(manifest.version, previous.manifest.version);
      if (order < 0) fail('HOST_DOWNGRADE', 'Downgrades require a separate recovery workflow.');
      if (order === 0 && digest !== previous.digest) fail('HOST_VERSION_REUSE', 'Changed manifest content must use a new version.');
    }
    const old = new Map(previous?.manifest.capabilities.map(c => [c.id, c]) ?? []);
    const token = this.#store(this.#reviews, {
      loaded, expectedGeneration: previous?.generation ?? null,
    }, REVIEW_TTL);
    return freeze({
      token, id: manifest.id, name: manifest.name, version: manifest.version, digest,
      scope: extensionScope(manifest), slots: [...new Set(manifest.contributions.cards.map(card => card.slot))].sort(),
      publisher: manifest.publisher, publisherVerified: false,
      package: loaded.package ?? null,
      mode: previous ? (previous.digest === digest ? 'review-grants' : 'update') : 'install',
      expiresAt: this.#clock() + REVIEW_TTL,
      capabilities: manifest.capabilities.map(c => ({
        ...c,
        currentlyGranted: previous?.grants.has(c.id) ?? false,
        newlyRequested: !old.has(c.id),
        newlyRequired: c.required && old.get(c.id)?.required === false,
      })),
      note: 'Experimental local JSON only. Publisher is unverified; no code or package signature is loaded.',
    });
  }

  cancelReview(token) { return this.#reviews.delete(token); }

  // Call only after a trusted host consent UI explicitly confirms this exact
  // digest and selected capability set. Manifest content cannot grant itself.
  confirmReview(token, selectedCapabilityIds) {
    const review = this.#take(this.#reviews, token);
    const { manifest, digest } = review.loaded;
    const previous = this.#entries.get(manifest.id);
    if ((previous?.generation ?? null) !== review.expectedGeneration)
      fail('HOST_STALE_REVIEW', 'Extension state changed; review the current version again.');
    if (!previous && this.#entries.size >= MAX_INSTALLED) fail('HOST_INSTALL_LIMIT', 'Experimental host extension limit reached.');
    const declared = new Set(manifest.capabilities.map(c => c.id));
    if (!Array.isArray(selectedCapabilityIds) || selectedCapabilityIds.length > declared.size ||
        new Set(selectedCapabilityIds).size !== selectedCapabilityIds.length ||
        selectedCapabilityIds.some(c => typeof c !== 'string' || !declared.has(c)))
      fail('HOST_GRANT', 'Selected grants must be unique capabilities declared by this manifest.');
    const grants = new Set(selectedCapabilityIds);
    if (manifest.capabilities.some(c => c.required && !grants.has(c.id)))
      fail('HOST_REQUIRED_DENIED', 'Required permission was declined; extension remains unchanged.');
    this.#entries.set(manifest.id, {
      manifest, digest, package: review.loaded.package ?? null,
      grants, enabled: true, generation: ++this.#generation,
      settings: retainSettings(manifest, previous?.settings),
    });
    return this.inspect(manifest.id);
  }

  #entry(id) {
    const entry = this.#entries.get(id);
    if (!entry) fail('HOST_NOT_FOUND', 'Extension is not installed in this host session.');
    return entry;
  }

  #state(entry) {
    if (this.#safeMode) return 'safe-mode';
    if (!entry.enabled) return 'disabled';
    if (entry.manifest.capabilities.some(c => c.required && !entry.grants.has(c.id))) return 'suspended';
    return 'active';
  }

  inspect(id) {
    const e = this.#entry(id);
    return freeze({
      id: e.manifest.id, name: e.manifest.name, version: e.manifest.version, digest: e.digest,
      state: this.#state(e), grants: [...e.grants].sort(), generation: e.generation,
      enabled: e.enabled, scope: extensionScope(e.manifest),
      slots: [...new Set(e.manifest.contributions.cards.map(card => card.slot))].sort(),
      publisher: e.manifest.publisher, publisherVerified: false,
      package: e.package,
    });
  }

  list() { return freeze([...this.#entries.keys()].sort().map(id => this.inspect(id))); }

  /** Plugin state is separate from permissions and launcher preferences. The
   * store calls restoreSettings only while reconstructing a private candidate. */
  settingsValues(id) { return freeze({ ...this.#entry(id).settings }); }
  restoreSettings(id, values) {
    const entry = this.#entry(id);
    validateSettingsPatch(entry.manifest, values, true);
    entry.settings = { ...defaultSettings(entry.manifest), ...values };
  }
  updateSettings(id, values) {
    const entry = this.#entry(id);
    if (this.#state(entry) !== 'active') fail('HOST_INACTIVE', 'Extension is inactive.');
    if (!entry.grants.has('storage.settings')) fail('HOST_PERMISSION', 'Plugin settings permission has not been granted.');
    validateSettingsPatch(entry.manifest, values);
    entry.settings = { ...entry.settings, ...values };
    entry.generation = ++this.#generation;
    return this.inspect(id);
  }

  revoke(id, capability) {
    const e = this.#entry(id);
    if (!e.manifest.capabilities.some(c => c.id === capability)) fail('HOST_GRANT', 'Capability is not declared by this extension.');
    e.grants.delete(capability);
    e.generation = ++this.#generation;
    return this.inspect(id);
  }

  disable(id) {
    const e = this.#entry(id);
    e.enabled = false; e.generation = ++this.#generation;
    return this.inspect(id);
  }

  enable(id) {
    const e = this.#entry(id);
    // Stored consent is retained. Missing required grants keep it suspended
    // until a fresh trusted permission review succeeds.
    e.enabled = true; e.generation = ++this.#generation;
    return this.inspect(id);
  }

  remove(id) {
    this.#entry(id);
    this.#entries.delete(id);
    ++this.#generation;
    for (const [token, review] of this.#reviews)
      if (review.loaded.manifest.id === id) this.#reviews.delete(token);
    for (const [token, action] of this.#actions)
      if (action.extensionId === id) this.#actions.delete(token);
    return freeze({ id, removed: true });
  }

  setSafeMode(enabled) {
    if (typeof enabled !== 'boolean') fail('HOST_STATE', 'Safe mode requires a boolean.');
    this.#safeMode = enabled;
    this.#consentEpoch++;
    // A leave/reenter of safe mode may not revive an old action or consent.
    this.#actions.clear(); this.#reviews.clear();
    return this.list();
  }

  // Host-only input. Opaque target binding stays private and never enters cards.
  // The current target changes before any asynchronous response is accepted.
  setContext(context) {
    if (context === null) this.#context = null;
    else {
      if (!context || typeof context !== 'object' ||
          !['rootId', 'instanceId', 'revision'].every(k => typeof context[k] === 'string' && context[k].length > 0 && context[k].length <= 512))
        fail('HOST_CONTEXT', 'Selected instance needs a host-owned root, instance, and revision binding.');
      const s = context.summary;
      if (!s || typeof s.minecraftVersion !== 'string' || typeof s.loader !== 'string' ||
          !SNAPSHOT.test(s.minecraftVersion) || !SNAPSHOT.test(s.loader) ||
          !Number.isSafeInteger(s.modCount) || s.modCount < 0 || s.modCount > 1_000_000 || typeof s.isolated !== 'boolean')
        fail('HOST_CONTEXT', 'Host summary has invalid values.');
      this.#context = freeze({
        rootId: context.rootId, instanceId: context.instanceId, revision: context.revision,
        summary: { minecraftVersion: s.minecraftVersion, loader: s.loader, modCount: s.modCount, isolated: s.isolated },
      });
    }
    this.#contextGeneration++;
  }

  #actionView(entry, action) {
    const granted = entry.grants.has(actionCapability(action));
    const selected = action.kind !== 'show-instance-summary' || this.#context !== null;
    return {
      id: action.id, label: action.label,
      enabled: granted && selected,
      unavailableReason: !granted ? 'permission-not-granted' : !selected ? 'no-selected-instance' : null,
    };
  }

  cards(slot, filter) {
    if (filter !== undefined && (!filter || typeof filter !== 'object' || Array.isArray(filter) ||
        Object.keys(filter).length !== 2 || !Object.hasOwn(filter, 'extensionId') || !Object.hasOwn(filter, 'cardId') ||
        typeof filter.extensionId !== 'string' || !filter.extensionId.length || filter.extensionId.length > 128 ||
        typeof filter.cardId !== 'string' || !filter.cardId.length || filter.cardId.length > 48))
      fail('HOST_FILTER', 'A panel filter must name one extension and one card.');
    const result = [];
    for (const e of this.#entries.values()) {
      if (this.#state(e) !== 'active') continue;
      if (filter && e.manifest.id !== filter.extensionId) continue;
      for (const card of e.manifest.contributions.cards) {
        if (card.slot !== slot) continue;
        if (filter && card.id !== filter.cardId) continue;
        result.push({
          extensionId: e.manifest.id, extensionName: e.manifest.name,
          publisherVerified: false, digest: e.digest,
          generation: e.generation, scope: extensionScope(e.manifest),
          id: card.id, title: card.title, text: card.text,
          ...(card.fields ? { fields: card.fields.map(key => ({
            ...e.manifest.settings.find(definition => definition.id === key), value: e.settings[key],
          })) } : {}),
          actions: card.actions.map(action => this.#actionView(e, action)),
        });
      }
    }
    return freeze(result);
  }

  /** A lightweight catalog carries identities and labels, never saved text or
   * render callbacks. The trusted UI alone creates views for these entries. */
  catalog() {
    const panels = [], actions = [];
    for (const entry of this.#entries.values()) {
      if (this.#state(entry) !== 'active') continue;
      for (const card of entry.manifest.contributions.cards) {
        if (card.slot !== 'rh-code.cards') continue;
        const identity = {
          extensionId: entry.manifest.id, extensionName: entry.manifest.name,
          cardId: card.id, digest: entry.digest, generation: entry.generation,
        };
        panels.push({ ...identity, viewId: panelViewId(entry.manifest.id, card.id), title: card.title });
        for (const action of card.actions)
          actions.push({ ...identity, cardTitle: card.title, kind: action.kind, ...this.#actionView(entry, action) });
      }
    }
    return freeze({ panels, actions });
  }

  /** Repeated field references amplify saved text across cards. Bound each
   * published surface below the protocol's 1 MB response envelope, with room
   * for context-dependent action availability and native envelope metadata. */
  validateProjectionBudget() {
    for (const slot of CARD_SLOTS)
      if (Buffer.byteLength(JSON.stringify(this.cards(slot)), 'utf8') > 768 * 1024)
        fail('HOST_PROJECTION_LIMIT', 'Plugin cards exceed the display budget; shorten text or remove repeated fields. Previous state retained.');
    if (Buffer.byteLength(JSON.stringify(this.catalog()), 'utf8') > 768 * 1024)
      fail('HOST_PROJECTION_LIMIT', 'Plugin component catalog exceeds the display budget. Previous state retained.');
  }

  prepareAction(extensionId, cardId, actionId) {
    const e = this.#entry(extensionId);
    if (this.#state(e) !== 'active') fail('HOST_INACTIVE', 'Extension is inactive.');
    const card = e.manifest.contributions.cards.find(c => c.id === cardId);
    const action = card?.actions.find(a => a.id === actionId);
    if (!action) fail('HOST_ACTION', 'Action was not declared by this extension.');
    const capability = actionCapability(action);
    if (!e.grants.has(capability)) fail('HOST_PERMISSION', 'Action permission has not been granted.');
    if (action.kind === 'show-instance-summary' && !this.#context) fail('HOST_CONTEXT', 'No instance is selected.');
    const token = this.#store(this.#actions, {
      extensionId, generation: e.generation, contextGeneration: this.#contextGeneration,
      action, capability,
    }, ACTION_TTL);
    return freeze({ token, extensionId, label: action.label, kind: action.kind });
  }

  cancelAction(token) { return this.#actions.delete(token); }

  commitAction(token) {
    const request = this.#take(this.#actions, token);
    const e = this.#entry(request.extensionId);
    if (this.#state(e) !== 'active' || e.generation !== request.generation || !e.grants.has(request.capability))
      fail('HOST_STALE_ACTION', 'Extension permissions or state changed before this action completed.');
    if (this.#contextGeneration !== request.contextGeneration)
      fail('HOST_STALE_CONTEXT', 'Selection changed; use the current card action again.');
    // Return only a fixed typed intent. There is no invoke/exec/fetch callback.
    const base = { kind: request.action.kind, extensionId: request.extensionId };
    switch (request.action.kind) {
      case 'navigate': return freeze({ ...base, target: request.action.target });
      case 'show-instance-summary': return freeze({ ...base, summary: { ...this.#context.summary } });
      case 'set-launcher-appearance': return freeze({ ...base, values: resolveAppearance(request.action.values, e.settings) });
      case 'copy-text': return freeze({ ...base, text: e.settings[request.action.setting] });
      default: fail('HOST_ACTION', 'Unsupported plugin action kind.');
    }
  }
}
