import { createHash } from 'node:crypto';
import { parseJson, fail, canonicalJson, freeze } from './json.mjs';

export const HOST_API = 1;
export const FORMAT = 'pcl-linux.declarative-extension';
export const CAPABILITIES = freeze({
  'ui.cards': { label: 'Add labeled text cards', data: 'No launcher data', risk: 'low' },
  'launcher.navigate': { label: 'Navigate to fixed launcher pages', data: 'No account, path, or process access', risk: 'low' },
  'instances.summary': { label: 'Show the selected instance summary', data: 'Minecraft version, loader, mod count, isolation flag; no name, path, account, or token', risk: 'read-only' },
});
export const NAVIGATION_TARGETS = freeze(['launch', 'instances', 'downloads', 'tools', 'settings']);
export const CARD_SLOTS = freeze(['home.secondary', 'tools.cards', 'rh-code.cards']);
export const EXTENSION_SCOPES = freeze(['launcher', 'rh-code', 'both']);
// Older manifests remain byte-for-byte authoritative. Reading their default
// scope must not insert a field and change a previously approved digest.
export const extensionScope = manifest => manifest.scope ?? 'launcher';
// JS `$` also matches just before a final line terminator. These contracts
// require the actual end of input, including for ASCII IDs and versions.
const ID = /^[a-z][a-z0-9-]*(?:\.[a-z][a-z0-9-]*){1,7}(?![\s\S])/;
const LOCAL_ID = /^[a-z][a-z0-9-]{0,47}(?![\s\S])/;
const VERSION = /^(0|[1-9]\d{0,5})\.(0|[1-9]\d{0,5})\.(0|[1-9]\d{0,5})(?![\s\S])/;
const UNSAFE_TEXT = /[\u0000-\u0008\u000b-\u001f\u007f-\u009f\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069\ud800-\udfff]/u;

function object(value, required, optional = []) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) fail('MANIFEST_OBJECT', 'Expected a manifest object.');
  if (Object.keys(value).some(key => ![...required, ...optional].includes(key)))
    fail('MANIFEST_UNKNOWN_FIELD', 'Unknown fields are not supported by this contract.');
  if (required.some(key => !Object.hasOwn(value, key))) fail('MANIFEST_MISSING_FIELD', 'A required manifest field is missing.');
}
function text(value, limit, multiline = false) {
  if (typeof value !== 'string' || value.trim() !== value || value.length === 0 || [...value].length > limit || UNSAFE_TEXT.test(value) || (!multiline && /[\n\t]/.test(value)))
    fail('MANIFEST_TEXT', 'Text must be nonempty, bounded, and free of control or direction-override characters.');
}
function array(value, limit, min = 0) {
  if (!Array.isArray(value) || value.length < min || value.length > limit) fail('MANIFEST_ARRAY', 'Manifest collection size is invalid.');
}
function unique(values) {
  if (new Set(values).size !== values.length) fail('MANIFEST_DUPLICATE_ID', 'Manifest identifiers must be unique within their scope.');
}
function id(value, pattern, max) {
  if (typeof value !== 'string' || value.length > max || !pattern.test(value)) fail('MANIFEST_ID', 'Invalid manifest identifier.');
}

/** Untrusted input is JSON text, never a plugin-supplied JavaScript object. */
export function readManifest(source) {
  const m = parseJson(source);
  object(m, ['format', 'schemaVersion', 'id', 'name', 'version', 'publisher', 'api', 'capabilities', 'contributions'], ['scope']);
  if (m.format !== FORMAT || m.schemaVersion !== 1) fail('MANIFEST_FORMAT', 'This is not a supported PCL RH declarative extension manifest.');
  id(m.id, ID, 128); text(m.name, 80); text(m.publisher, 80);
  if (Object.hasOwn(m, 'scope') && !EXTENSION_SCOPES.includes(m.scope))
    fail('MANIFEST_SCOPE', 'Extension scope must be launcher, rh-code, or both.');
  if (typeof m.version !== 'string' || !VERSION.test(m.version)) fail('MANIFEST_VERSION', 'Use three bounded stable version numbers, such as 1.0.0.');
  object(m.api, ['min', 'maxExclusive']);
  if (![m.api.min, m.api.maxExclusive].every(n => Number.isSafeInteger(n) && n >= 1 && n <= 1000) || m.api.min >= m.api.maxExclusive)
    fail('MANIFEST_API', 'API compatibility uses an ordered integer interval.');
  if (HOST_API < m.api.min || HOST_API >= m.api.maxExclusive) fail('MANIFEST_INCOMPATIBLE', 'This extension does not support the current experimental host API.');
  array(m.capabilities, Object.keys(CAPABILITIES).length, 1);
  for (const c of m.capabilities) {
    object(c, ['id', 'required', 'reason']);
    if (typeof c.id !== 'string' || !Object.hasOwn(CAPABILITIES, c.id)) fail('CAPABILITY_UNKNOWN', 'The manifest requests an unsupported capability.');
    if (typeof c.required !== 'boolean') fail('CAPABILITY_REQUIRED', 'Capability required must be an explicit boolean.');
    text(c.reason, 240);
  }
  unique(m.capabilities.map(c => c.id));
  const declared = new Map(m.capabilities.map(c => [c.id, c]));
  if (!declared.get('ui.cards')?.required) fail('CAPABILITY_CARDS_REQUIRED', 'Text-card permission is required for this format.');
  object(m.contributions, ['cards']);
  array(m.contributions.cards, 12, 1);
  unique(m.contributions.cards.map(card => card?.id));
  for (const card of m.contributions.cards) {
    object(card, ['id', 'slot', 'title', 'text', 'actions']);
    id(card.id, LOCAL_ID, 48); text(card.title, 100); text(card.text, 2000, true);
    if (!CARD_SLOTS.includes(card.slot)) fail('CONTRIBUTION_SLOT', 'Contributions may only use an additive, host-owned card slot.');
    const slotScope = card.slot === 'rh-code.cards' ? 'rh-code' : 'launcher';
    if (extensionScope(m) !== 'both' && extensionScope(m) !== slotScope)
      fail('CONTRIBUTION_SCOPE', 'This card slot is outside the extension running scope.');
    array(card.actions, 4); unique(card.actions.map(action => action?.id));
    for (const action of card.actions) {
      object(action, ['id', 'label', 'kind'], ['target']);
      id(action.id, LOCAL_ID, 48); text(action.label, 64);
      const capability = actionCapability(action);
      if (!declared.has(capability)) fail('CAPABILITY_UNDECLARED', 'An action uses a capability that was not declared.');
      if (action.kind === 'navigate' && !NAVIGATION_TARGETS.includes(action.target)) fail('ACTION_TARGET', 'Navigation must use a fixed host page.');
      if (action.kind === 'show-instance-summary' && Object.hasOwn(action, 'target')) fail('ACTION_TARGET', 'Instance summary actions cannot supply a target or file path.');
    }
  }
  const digest = createHash('sha256').update(canonicalJson(m)).digest('hex');
  return freeze({ manifest: m, digest });
}

export function actionCapability(action) {
  if (action.kind === 'navigate') return 'launcher.navigate';
  if (action.kind === 'show-instance-summary') return 'instances.summary';
  fail('ACTION_KIND', 'Only fixed navigation and host-generated instance summaries are supported.');
}

export function compareVersions(left, right) {
  const a = left.split('.').map(Number), b = right.split('.').map(Number);
  for (let i = 0; i < 3; i++) if (a[i] !== b[i]) return Math.sign(a[i] - b[i]);
  return 0;
}
