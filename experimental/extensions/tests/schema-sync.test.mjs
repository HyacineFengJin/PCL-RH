/** The editor schema is structural guidance; runtime admission owns consent,
 * cross-references and complete range checks. This bounded test evaluator
 * covers only keywords used here and fails when a new keyword needs support. */
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile, readdir } from 'node:fs/promises';
import { CAPABILITIES, CARD_SLOTS, EXTENSION_SCOPES, NAVIGATION_TARGETS, readManifest } from '../src/manifest.mjs';
import { APPEARANCE_FIELDS, DOWNLOAD_FIELDS } from '../src/settings.mjs';
import { readPackage } from '../src/package.mjs';

const schema = JSON.parse(await readFile(new URL('../schema/manifest.schema.json', import.meta.url), 'utf8'));
const card = schema.properties.contributions.properties.cards.items;
const action = kind => card.properties.actions.items.oneOf.find(item => item.properties.kind.const === kind);
const widget = kind => card.properties.widgets.items.oneOf.find(item => item.properties.kind.const === kind);
const keywords = new Set(['$schema', 'title', 'description', 'type', 'const', 'enum', 'pattern', 'minLength', 'maxLength',
  'minimum', 'maximum', 'minItems', 'maxItems', 'uniqueItems', 'items', 'minProperties', 'maxProperties',
  'properties', 'required', 'additionalProperties', 'oneOf', 'allOf', 'if', 'then']);
function inspectKeywords(rule) {
  for (const key of Object.keys(rule)) assert.ok(keywords.has(key), `Add test support for schema keyword ${key}`);
  for (const child of Object.values(rule.properties ?? {})) inspectKeywords(child);
  if (rule.items) inspectKeywords(rule.items);
  for (const child of [...(rule.oneOf ?? []), ...(rule.allOf ?? []), ...[rule.if, rule.then].filter(Boolean)]) inspectKeywords(child);
}
function errors(rule, value, at = '$') {
  const result = [], reject = reason => result.push(`${at}: ${reason}`);
  const types = { object: value !== null && typeof value === 'object' && !Array.isArray(value),
    array: Array.isArray(value), string: typeof value === 'string', boolean: typeof value === 'boolean',
    integer: Number.isSafeInteger(value) };
  if (rule.type && !types[rule.type]) return [`${at}: expected ${rule.type}`];
  if (Object.hasOwn(rule, 'const') && !Object.is(value, rule.const)) reject('const');
  if (rule.enum && !rule.enum.some(allowed => Object.is(value, allowed))) reject('enum');
  if (typeof value === 'string') {
    const length = [...value].length;
    if (rule.minLength !== undefined && length < rule.minLength) reject('minLength');
    if (rule.maxLength !== undefined && length > rule.maxLength) reject('maxLength');
    if (rule.pattern && !new RegExp(rule.pattern, 'u').test(value)) reject('pattern');
  }
  if (typeof value === 'number') {
    if (rule.minimum !== undefined && value < rule.minimum) reject('minimum');
    if (rule.maximum !== undefined && value > rule.maximum) reject('maximum');
  }
  if (Array.isArray(value)) {
    if (rule.minItems !== undefined && value.length < rule.minItems) reject('minItems');
    if (rule.maxItems !== undefined && value.length > rule.maxItems) reject('maxItems');
    if (rule.uniqueItems && new Set(value.map(item => JSON.stringify(item))).size !== value.length) reject('uniqueItems');
    if (rule.items) value.forEach((item, index) => result.push(...errors(rule.items, item, `${at}[${index}]`)));
  } else if (value !== null && typeof value === 'object') {
    const keys = Object.keys(value);
    if (rule.minProperties !== undefined && keys.length < rule.minProperties) reject('minProperties');
    if (rule.maxProperties !== undefined && keys.length > rule.maxProperties) reject('maxProperties');
    for (const key of rule.required ?? []) if (!Object.hasOwn(value, key)) reject(`required ${key}`);
    if (rule.additionalProperties === false)
      for (const key of keys) if (!Object.hasOwn(rule.properties ?? {}, key)) reject(`additional property ${key}`);
    for (const [key, child] of Object.entries(rule.properties ?? {}))
      if (Object.hasOwn(value, key)) result.push(...errors(child, value[key], `${at}.${key}`));
  }
  if (rule.oneOf && rule.oneOf.filter(child => errors(child, value, at).length === 0).length !== 1) reject('oneOf');
  for (const child of rule.allOf ?? []) result.push(...errors(child, value, at));
  if (rule.if && errors(rule.if, value, at).length === 0 && rule.then) result.push(...errors(rule.then, value, at));
  return result;
}
const valid = value => assert.deepEqual(errors(schema, value), []);
function base() {
  return {
    format: 'pcl-linux.declarative-extension', schemaVersion: 1, id: 'local.schema-fixture', name: 'Schema fixture',
    version: '1.0.0', publisher: 'Fixture', api: { min: 1, maxExclusive: 2 },
    capabilities: [{ id: 'ui.cards', required: true, reason: 'Display card.' }],
    contributions: { cards: [{ id: 'card', slot: 'home.banner', title: 'Card', text: 'Body', actions: [] }] },
  };
}
function withWidget(value) { const m = base(); m.contributions.cards[0].widgets = [value]; return m; }
const notice = () => ({ id: 'notice', kind: 'notice', text: 'Saved choices\nPlain text', tone: 'info' });
const valueWidget = () => ({ id: 'value', kind: 'value', label: 'Saved value', setting: 'choice', suffix: 'MiB/s' });
const progress = () => ({ id: 'progress', kind: 'progress', label: 'Progress', setting: 'amount', max: 100 });
const downloads = values => {
  const m = base();
  m.contributions.cards[0].actions = [{ id: 'apply', label: 'Apply', kind: 'set-launcher-downloads', values }];
  return m;
};

test('editor schema capability, slot, page, scope and effect enums match the production runtime contract', () => {
  inspectKeywords(schema);
  assert.deepEqual(schema.properties.capabilities.items.properties.id.enum, Object.keys(CAPABILITIES));
  assert.equal(schema.properties.capabilities.maxItems, Object.keys(CAPABILITIES).length);
  assert.deepEqual(card.properties.slot.enum, CARD_SLOTS);
  assert.deepEqual(action('navigate').properties.target.enum, NAVIGATION_TARGETS);
  assert.deepEqual(schema.properties.scope.enum, EXTENSION_SCOPES);
  assert.deepEqual(card.properties.presentation.enum, ['card', 'compact']);
  assert.deepEqual(card.required, ['id', 'slot', 'title', 'text', 'actions']);
  assert.equal(Object.hasOwn(card.properties.presentation, 'default'), false);
  assert.equal(Object.hasOwn(card.properties.widgets, 'default'), false);
  assert.equal(card.properties.widgets.maxItems, 8);
  assert.equal(card.properties.fields.maxItems, 8);
  assert.equal(card.properties.actions.maxItems, 4);
  assert.deepEqual(card.properties.widgets.items.oneOf.map(item => item.properties.kind.const), ['notice', 'value', 'progress']);
  assert.deepEqual(card.properties.actions.items.oneOf.map(item => item.properties.kind.const),
    ['navigate', 'show-instance-summary', 'set-launcher-appearance', 'set-launcher-downloads', 'copy-text']);
  const appearance = action('set-launcher-appearance').properties.values.properties;
  assert.deepEqual(Object.keys(appearance), Object.keys(APPEARANCE_FIELDS));
  for (const [field, allowed] of Object.entries(APPEARANCE_FIELDS)) {
    if (allowed === 'boolean') assert.equal(appearance[field].oneOf[0].type, 'boolean');
    else assert.deepEqual(appearance[field].oneOf[0].enum, allowed);
  }
  const limits = action('set-launcher-downloads').properties.values;
  assert.equal(limits.minProperties, 1); assert.equal(limits.maxProperties, 2);
  assert.deepEqual(Object.keys(limits.properties), Object.keys(DOWNLOAD_FIELDS));
  for (const [field, range] of Object.entries(DOWNLOAD_FIELDS))
    assert.deepEqual(limits.properties[field].oneOf[0], { type: 'integer', minimum: range.min, maximum: range.max });
});

test('structural schema applies launcher scope to all additive slots and preserves the missing-scope default', () => {
  for (const slot of CARD_SLOTS) for (const scope of [undefined, ...EXTENSION_SCOPES]) {
    const m = base(); m.contributions.cards[0].slot = slot;
    if (scope !== undefined) m.scope = scope;
    const accepted = scope === 'both' || (scope === 'rh-code') === (slot === 'rh-code.cards');
    assert.equal(errors(schema, m).length === 0, accepted, `${scope ?? 'default'} / ${slot}`);
    if (accepted) readManifest(JSON.stringify(m));
    else assert.throws(() => readManifest(JSON.stringify(m)), { code: 'CONTRIBUTION_SCOPE' });
  }
});

test('structural widget shapes reject missing, unknown, mixed and out-of-bound fields', () => {
  for (const sample of [notice(), valueWidget(), progress()]) {
    valid(withWidget(sample));
    for (const required of widget(sample.kind).required) {
      const broken = { ...sample }; delete broken[required];
      assert.ok(errors(schema, withWidget(broken)).length, `${sample.kind} requires ${required}`);
    }
    assert.ok(errors(schema, withWidget({ ...sample, unknown: true })).length);
  }
  const invalid = [
    { ...notice(), tone: 'danger' }, { ...notice(), text: '' }, { ...notice(), text: '🦊'.repeat(1001) },
    { ...notice(), setting: 'choice' }, { ...valueWidget(), label: '' }, { ...valueWidget(), label: '🦊'.repeat(101) },
    { ...valueWidget(), suffix: '' }, { ...valueWidget(), suffix: '🦊'.repeat(25) }, { ...valueWidget(), setting: 'choice\n' },
    { ...valueWidget(), value: 'undeclared' }, { ...progress(), max: 0 }, { ...progress(), max: 1000001 },
    { ...progress(), max: 1.5 }, { ...progress(), max: '100' }, { ...progress(), suffix: 'extra' },
    { id: 'unknown', kind: 'html', text: 'No renderer' },
  ];
  for (const sample of invalid) assert.ok(errors(schema, withWidget(sample)).length, JSON.stringify(sample));
  const legal = withWidget({ ...notice(), text: '🦊'.repeat(1000) }); valid(legal); readManifest(JSON.stringify(legal));
  const tooMany = base(); tooMany.contributions.cards[0].widgets = Array.from({ length: 9 }, (_, i) => ({ ...notice(), id: `notice-${i}` }));
  assert.ok(errors(schema, tooMany).length);
  for (const presentation of ['card', 'compact']) { const m = base(); m.contributions.cards[0].presentation = presentation; valid(m); }
  for (const presentation of [null, false, 'html']) { const m = base(); m.contributions.cards[0].presentation = presentation; assert.ok(errors(schema, m).length); }
});

test('structural downloads schema admits only exact constants or one-setting bindings within integer bounds', () => {
  for (const values of [
    { max_concurrent_transfers: 1 }, { max_concurrent_transfers: 64 }, { total_rate_limit_mib_per_second: 0 },
    { total_rate_limit_mib_per_second: 1024 }, { max_concurrent_transfers: { setting: 'transfers' }, total_rate_limit_mib_per_second: { setting: 'rate' } },
  ]) valid(downloads(values));
  for (const values of [null, [], {},
    { max_concurrent_transfers: 0 }, { max_concurrent_transfers: 65 }, { max_concurrent_transfers: 1.5 },
    { max_concurrent_transfers: null }, { max_concurrent_transfers: '4' }, { max_concurrent_transfers: true },
    { total_rate_limit_mib_per_second: -1 }, { total_rate_limit_mib_per_second: 1025 },
    { max_concurrent_transfers: { setting: 'transfers', extra: 3 } }, { max_concurrent_transfers: { setting: 'bad\n' } },
    { proxy: 'https://example.invalid' }, { max_concurrent_transfers: 4, total_rate_limit_mib_per_second: 0, theme: 'dark' },
  ]) assert.ok(errors(schema, downloads(values)).length, JSON.stringify(values));
  const extra = downloads({ max_concurrent_transfers: 4 }); extra.contributions.cards[0].actions[0].target = 'settings.manage';
  assert.ok(errors(schema, extra).length);
});

test('schema describes runtime-only setting references and range containment without weakening runtime checks', () => {
  assert.match(schema.description, /setting references.*setting types.*range compatibility/);
  assert.match(card.properties.widgets.description, /Runtime.*unique widget IDs.*range containment/);
  assert.match(action('set-launcher-downloads').properties.values.description, /runtime validates/);
  const unresolved = withWidget(valueWidget()); valid(unresolved);
  assert.throws(() => readManifest(JSON.stringify(unresolved)), { code: 'CONTRIBUTION_WIDGET' });
  const range = downloads({ max_concurrent_transfers: { setting: 'transfers' } });
  range.capabilities.push({ id: 'storage.settings', required: true, reason: 'Save choice.' },
    { id: 'launcher.downloads', required: false, reason: 'Apply limits.' });
  range.settings = [{ id: 'transfers', label: 'Transfers', type: 'number', default: 4, min: 1, max: 65 }];
  valid(range); assert.throws(() => readManifest(JSON.stringify(range)), { code: 'ACTION_BINDING' });
});

test('every distributable manifest and package example passes the editor structure and production runtime parser', async () => {
  const directory = new URL('../examples/', import.meta.url), names = (await readdir(directory)).filter(name => name.endsWith('.json')).sort();
  assert.ok(names.includes('instance-compass.json') && names.includes('instance-compass.pclext.json'));
  const kinds = new Set(), widgetKinds = new Set(), slots = new Set();
  for (const name of names) {
    const text = await readFile(new URL(name, directory), 'utf8'), document = JSON.parse(text);
    const loaded = document.packageFormat ? readPackage(text) : readManifest(text);
    assert.deepEqual(errors(schema, loaded.manifest), [], name);
    for (const sample of loaded.manifest.contributions.cards) {
      slots.add(sample.slot);
      for (const operation of sample.actions) kinds.add(operation.kind);
      for (const display of sample.widgets ?? []) widgetKinds.add(display.kind);
    }
  }
  assert.deepEqual([...kinds].sort(), card.properties.actions.items.oneOf.map(item => item.properties.kind.const).sort());
  assert.deepEqual([...widgetKinds].sort(), card.properties.widgets.items.oneOf.map(item => item.properties.kind.const).sort());
  assert.deepEqual([...slots].sort(), [...CARD_SLOTS].sort());
});
