/** Typed plugin-owned state and fixed launcher effect arguments. These helpers
 * validate both declarations and saved values; there is no expression evaluator
 * or executable callback hidden inside a binding. */
import { fail } from './json.mjs';

const LOCAL_ID = /^[a-z][a-z0-9-]{0,47}(?![\s\S])/;
const UNSAFE = /[\u0000-\u0008\u000b-\u001f\u007f-\u009f\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069\ud800-\udfff]/u;
const TYPES = ['text', 'toggle', 'select', 'number'];
const MAX_NUMBER = 1_000_000;
export const APPEARANCE_FIELDS = Object.freeze({
  theme: ['system', 'light', 'dark'],
  custom_cursor: 'boolean',
  cursor_style: ['accent', 'outline'],
  cursor_size: ['small', 'standard'],
  custom_context_menu: 'boolean',
  context_menu_density: ['comfortable', 'compact'],
});
export const DOWNLOAD_FIELDS = Object.freeze({
  max_concurrent_transfers: Object.freeze({ min: 1, max: 64 }),
  total_rate_limit_mib_per_second: Object.freeze({ min: 0, max: 1024 }),
});

function object(value, required, optional = []) {
  if (!value || typeof value !== 'object' || Array.isArray(value) ||
      required.some(key => !Object.hasOwn(value, key)) ||
      Object.keys(value).some(key => ![...required, ...optional].includes(key)))
    fail('SETTINGS_SHAPE', 'Plugin settings contain unsupported or missing fields.');
}
function label(value, max = 100) {
  if (typeof value !== 'string' || !value.trim() || value.trim() !== value ||
      [...value].length > max || UNSAFE.test(value) || /[\n\t]/.test(value))
    fail('SETTINGS_LABEL', 'Plugin setting labels must be bounded plain text.');
}
function integer(value) {
  return Number.isSafeInteger(value) && Math.abs(value) <= MAX_NUMBER;
}

export function validateSettingValue(definition, value) {
  let valid = false;
  switch (definition.type) {
    case 'text':
      valid = typeof value === 'string' && [...value].length <= definition.maxLength &&
        !UNSAFE.test(value) && (definition.multiline === true || !/[\n\t]/.test(value));
      break;
    case 'toggle': valid = typeof value === 'boolean'; break;
    case 'select': valid = typeof value === 'string' && definition.options.some(option => option.value === value); break;
    case 'number': valid = integer(value) && value >= definition.min && value <= definition.max; break;
  }
  if (!valid) fail('SETTINGS_VALUE', 'A plugin setting is outside its declared type or limits.');
  return value;
}

export function validateSettings(definitions) {
  if (!Array.isArray(definitions) || definitions.length > 16)
    fail('SETTINGS_LIMIT', 'A plugin may declare at most 16 settings.');
  const seen = new Set();
  for (const definition of definitions) {
    if (!definition || !TYPES.includes(definition.type)) fail('SETTINGS_TYPE', 'Unsupported plugin setting type.');
    const optional = { text: ['maxLength', 'multiline'], toggle: [], select: ['options'], number: ['min', 'max'] }[definition.type];
    object(definition, ['id', 'label', 'type', 'default'], optional);
    if (typeof definition.id !== 'string' || !LOCAL_ID.test(definition.id) || seen.has(definition.id))
      fail('SETTINGS_ID', 'Plugin setting identifiers must be unique local IDs.');
    seen.add(definition.id); label(definition.label);
    if (definition.type === 'text' && (!Number.isSafeInteger(definition.maxLength) || definition.maxLength < 1 ||
        definition.maxLength > 4000 || (Object.hasOwn(definition, 'multiline') && typeof definition.multiline !== 'boolean')))
      fail('SETTINGS_LIMIT', 'Text settings need a maximum length from 1 to 4000.');
    if (definition.type === 'number' && (!integer(definition.min) || !integer(definition.max) || definition.min > definition.max))
      fail('SETTINGS_LIMIT', 'Number settings need an ordered bounded integer range.');
    if (definition.type === 'select') {
      if (!Array.isArray(definition.options) || definition.options.length < 2 || definition.options.length > 16)
        fail('SETTINGS_LIMIT', 'Select settings need between 2 and 16 options.');
      const values = new Set();
      for (const option of definition.options) {
        object(option, ['value', 'label']); label(option.value, 64); label(option.label);
        if (values.has(option.value)) fail('SETTINGS_OPTION', 'Select option values must be unique.');
        values.add(option.value);
      }
    }
    validateSettingValue(definition, definition.default);
  }
  return definitions;
}

export function defaultSettings(manifest) {
  return Object.fromEntries((manifest.settings ?? []).map(definition => [definition.id, definition.default]));
}

export function validateSettingsPatch(manifest, values, allowEmpty = false) {
  const definitions = new Map((manifest.settings ?? []).map(definition => [definition.id, definition]));
  if (!values || typeof values !== 'object' || Array.isArray(values) ||
      Object.keys(values).length > 16 || (!allowEmpty && Object.keys(values).length === 0))
    fail('SETTINGS_PATCH', 'Expected a bounded plugin settings patch.');
  for (const [key, value] of Object.entries(values)) {
    if (!definitions.has(key)) fail('SETTINGS_KEY', 'A plugin settings patch contains an undeclared key.');
    validateSettingValue(definitions.get(key), value);
  }
  return values;
}

/** Updates retain values only where the new declaration still accepts them.
 * A changed type or removed option falls back to the new declared default. */
export function retainSettings(manifest, previous = {}) {
  const values = defaultSettings(manifest);
  for (const definition of manifest.settings ?? []) {
    if (!Object.hasOwn(previous, definition.id)) continue;
    try { values[definition.id] = validateSettingValue(definition, previous[definition.id]); } catch {}
  }
  return values;
}

function acceptsAppearance(field, value) {
  const allowed = APPEARANCE_FIELDS[field];
  return allowed === 'boolean' ? typeof value === 'boolean' : allowed.includes(value);
}

export function validateAppearanceAction(values, definitions) {
  if (!values || typeof values !== 'object' || Array.isArray(values) ||
      Object.keys(values).length < 1 || Object.keys(values).length > 6)
    fail('ACTION_APPEARANCE', 'Appearance actions need a bounded set of supported fields.');
  for (const [field, value] of Object.entries(values)) {
    if (!Object.hasOwn(APPEARANCE_FIELDS, field)) fail('ACTION_APPEARANCE', 'Unsupported launcher appearance field.');
    if (value && typeof value === 'object') {
      object(value, ['setting']);
      const definition = definitions.find(definition => definition.id === value.setting);
      const compatible = APPEARANCE_FIELDS[field] === 'boolean' ? definition?.type === 'toggle' :
        definition?.type === 'select' && definition.options.every(option => acceptsAppearance(field, option.value));
      if (!compatible) fail('ACTION_BINDING', 'An appearance binding must match the destination type and allowed values.');
    } else if (!acceptsAppearance(field, value)) fail('ACTION_APPEARANCE', 'Unsupported launcher appearance value.');
  }
}

export function resolveAppearance(values, settings) {
  return Object.fromEntries(Object.entries(values).map(([field, value]) =>
    [field, value && typeof value === 'object' ? settings[value.setting] : value]));
}

/** Check the whole binding range at consent time, so a later valid settings
 * save cannot broaden the two preference fields approved by this action. */
export function validateDownloadsAction(values, definitions) {
  if (!values || typeof values !== 'object' || Array.isArray(values) ||
      Object.keys(values).length < 1 || Object.keys(values).length > 2)
    fail('ACTION_DOWNLOADS', 'Download actions need one or two supported limit fields.');
  for (const [field, value] of Object.entries(values)) {
    if (!Object.hasOwn(DOWNLOAD_FIELDS, field)) fail('ACTION_DOWNLOADS', 'Unsupported launcher download field.');
    const range = DOWNLOAD_FIELDS[field];
    if (value && typeof value === 'object') {
      object(value, ['setting']);
      const definition = definitions.find(definition => definition.id === value.setting);
      if (definition?.type !== 'number' || definition.min < range.min || definition.max > range.max)
        fail('ACTION_BINDING', 'A download binding must fit the destination integer range.');
    } else if (!Number.isSafeInteger(value) || value < range.min || value > range.max)
      fail('ACTION_DOWNLOADS', 'Unsupported launcher download limit.');
  }
}

export function resolveDownloads(values, settings) {
  return Object.fromEntries(Object.entries(values).map(([field, value]) =>
    [field, value && typeof value === 'object' ? settings[value.setting] : value]));
}
