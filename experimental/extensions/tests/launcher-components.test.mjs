/** Production host/store fixtures exercise declarations, durable saved values
 * and typed intents without loading Agent code or applying launcher effects. */
import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { cp, copyFile, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { CARD_SLOTS, NAVIGATION_TARGETS, readManifest, actionCapability } from '../src/manifest.mjs';
import { ExtensionHost } from '../src/host.mjs';
import { ExtensionStore } from '../src/store.mjs';
import { createAuthorManifest } from '../src/authoring.mjs';
import { createPackage, readPackage } from '../src/package.mjs';
import { DOWNLOAD_FIELDS, validateDownloadsAction, resolveDownloads } from '../src/settings.mjs';

const scratch = fileURLToPath(new URL('../../../work/plugin-launcher-surfaces-2026-10-20/', import.meta.url));
const newSlots = ['home.banner', 'home.quick-actions', 'settings.launch', 'settings.java', 'settings.manage', 'settings.language', 'settings.misc'];
const newTargets = ['settings.launch', 'settings.java', 'settings.manage', 'settings.personalize', 'settings.language', 'settings.misc'];
const expected = entry => ({ digest: entry.digest, generation: entry.generation });
const source = manifest => JSON.stringify(manifest);
const contractError = { name: 'ContractError' };
function manifest() {
  return {
    format: 'pcl-linux.declarative-extension', schemaVersion: 1,
    id: 'local.launcher-components', name: 'Launcher components', version: '1.0.0', publisher: 'Fixture', scope: 'both',
    api: { min: 1, maxExclusive: 2 },
    capabilities: [
      { id: 'ui.cards', required: true, reason: 'Display host cards.' },
      { id: 'storage.settings', required: true, reason: 'Save declared choices.' },
      { id: 'launcher.downloads', required: false, reason: 'Apply saved download limits on click.' },
      { id: 'launcher.navigate', required: false, reason: 'Open fixed host pages.' },
    ],
    settings: [
      { id: 'transfers', label: 'Transfers', type: 'number', min: 1, max: 64, default: 4 },
      { id: 'rate', label: 'Rate', type: 'number', min: 0, max: 1024, default: 0 },
      { id: 'note', label: 'Note', type: 'text', maxLength: 4000, multiline: true, default: 'PRIVATE SAVED NOTE' },
      { id: 'ready', label: 'Ready', type: 'toggle', default: false },
      { id: 'stage', label: 'Stage', type: 'select', default: 'plan', options: [
        { value: 'plan', label: 'Plan' }, { value: 'build', label: 'Build' },
      ] },
    ],
    contributions: { cards: [{
      id: 'dashboard', slot: 'home.banner', title: 'Saved launcher choices', text: 'Explicit user controls.',
      presentation: 'compact', fields: ['transfers', 'rate', 'note'],
      widgets: [
        { id: 'notice', kind: 'notice', text: 'Line one\nLine two ${note} <strong>literal</strong>', tone: 'info' },
        { id: 'transfers', kind: 'value', label: 'Transfers', setting: 'transfers', suffix: 'connections' },
        { id: 'rate', kind: 'progress', label: 'Rate', setting: 'rate', max: 1024 },
        { id: 'note', kind: 'value', label: 'Note', setting: 'note' },
        { id: 'ready', kind: 'value', label: 'Ready', setting: 'ready' },
        { id: 'stage', kind: 'value', label: 'Stage', setting: 'stage' },
      ],
      actions: [{ id: 'apply', label: 'Apply download limits', kind: 'set-launcher-downloads', values: {
        max_concurrent_transfers: { setting: 'transfers' }, total_rate_limit_mib_per_second: { setting: 'rate' },
      } }],
    }] },
  };
}
function altered(mutate) { const m = manifest(); mutate(m); return source(m); }
function installHost(host, m = manifest(), grants) {
  const review = host.prepareReview(source(m));
  return host.confirmReview(review.token, grants ?? review.capabilities.map(capability => capability.id));
}
async function install(store, m = manifest(), grants) {
  const review = store.prepareReview(source(m));
  return store.confirmReview(review.token, grants ?? review.capabilities.map(capability => capability.id));
}
async function directory(t) {
  await mkdir(scratch, { recursive: true });
  const root = await mkdtemp(path.join(scratch, 'runtime-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  return root;
}
async function fixture(t) {
  const root = await directory(t), file = path.join(root, 'extensions.json');
  const store = await ExtensionStore.open({ file }), entry = await install(store);
  return { root, file, store, id: entry.id };
}
function amplified(defaultValue = '') {
  const m = manifest();
  m.settings = [{ id: 'note', label: 'Note', type: 'text', maxLength: 4000, default: defaultValue }];
  m.contributions.cards = Array.from({ length: 12 }, (_, index) => ({
    id: `card-${index}`, slot: 'home.quick-actions', title: 'Values', text: 'Saved values.', actions: [],
    widgets: Array.from({ length: 8 }, (_, widget) => ({ id: `value-${widget}`, kind: 'value', label: 'Note', setting: 'note' })),
  }));
  return m;
}

test('legacy manifest digest and entire absent-widget card projection remain exact', async () => {
  const legacy = await readFile(new URL('../examples/instance-compass.json', import.meta.url), 'utf8');
  const digest = '8242e3a1f4e84e04e090d7e5e3a1561d8e73f0f004cb3082805e83af995e8b56';
  const parsed = readManifest(legacy), host = new ExtensionHost();
  assert.equal(parsed.digest, digest);
  assert.equal(Object.hasOwn(parsed.manifest, 'scope'), false);
  const review = host.prepareReview(legacy); host.confirmReview(review.token, review.capabilities.map(c => c.id));
  assert.deepEqual(host.cards('tools.cards'), [{
    extensionId: 'local.instance-compass', extensionName: '实例罗盘 · Instance Compass', publisherVerified: false,
    digest, generation: 1, scope: 'launcher', id: 'compass', title: '出发之前，看一眼实例',
    text: '用一个小卡片回到实例选择，或查看由宿主生成的只读摘要。所有按钮都需要对应的授权。',
    actions: [
      { id: 'instances', label: '选择实例', enabled: true, unavailableReason: null },
      { id: 'summary', label: '查看实例摘要', enabled: false, unavailableReason: 'no-selected-instance' },
    ],
  }]);
  assert.equal(readPackage(createPackage(legacy)).digest, digest);
});

test('all additive slots and fixed page targets validate through the authoring helper and actual host', () => {
  assert.deepEqual(CARD_SLOTS, ['home.secondary', 'tools.cards', 'rh-code.cards', 'settings.cards', ...newSlots]);
  assert.deepEqual(NAVIGATION_TARGETS, ['launch', 'instances', 'downloads', 'tools', 'settings', ...newTargets]);
  for (const slot of CARD_SLOTS) for (const target of NAVIGATION_TARGETS) {
    const m = createAuthorManifest({
      id: 'local.author-check', name: 'Author', version: '1.0.0', publisher: 'Fixture', slot,
      title: 'Card', text: 'Body', navigationEnabled: true, navigationTarget: target, navigationLabel: 'Open', summaryEnabled: false,
    });
    const host = new ExtensionHost(), entry = installHost(host, m);
    assert.equal(host.cards(slot).length, 1);
    assert.deepEqual(host.commitAction(host.prepareAction(entry.id, 'main-card', 'navigate').token),
      { kind: 'navigate', extensionId: entry.id, target });
    if (slot !== 'rh-code.cards') {
      m.scope = 'rh-code'; assert.throws(() => readManifest(source(m)), { code: 'CONTRIBUTION_SCOPE' });
    }
  }
  assert.throws(() => createAuthorManifest({ id: 'local.bad', version: '1.0.0', slot: 'settings.network' }));
  assert.throws(() => readManifest(altered(m => m.contributions.cards[0].slot = 'settings.network')), { code: 'CONTRIBUTION_SLOT' });
  assert.throws(() => readManifest(altered(m => m.contributions.cards[0].actions = [
    { id: 'go', label: 'Open', kind: 'navigate', target: 'settings.accounts' },
  ])), { code: 'ACTION_TARGET' });
});

test('optional presentation and empty widgets retain explicit presence without adding defaults', () => {
  for (const presentation of ['card', 'compact']) {
    const m = manifest(); m.contributions.cards[0].presentation = presentation; m.contributions.cards[0].widgets = [];
    const host = new ExtensionHost(); installHost(host, m);
    assert.equal(host.cards('home.banner')[0].presentation, presentation);
    assert.deepEqual(host.cards('home.banner')[0].widgets, []);
  }
  const m = manifest(); delete m.contributions.cards[0].presentation; delete m.contributions.cards[0].widgets;
  const host = new ExtensionHost(); installHost(host, m);
  assert.equal(Object.hasOwn(host.cards('home.banner')[0], 'presentation'), false);
  assert.equal(Object.hasOwn(host.cards('home.banner')[0], 'widgets'), false);
  for (const presentation of [null, '', false, 'html'])
    assert.throws(() => readManifest(altered(m => m.contributions.cards[0].presentation = presentation)), { code: 'CONTRIBUTION_PRESENTATION' });
});

test('all widget projections contain only resolved primitives and preserve literal notice text', () => {
  const host = new ExtensionHost(), m = manifest(); installHost(host, m);
  const card = host.cards('home.banner')[0];
  assert.deepEqual(card.widgets, [
    m.contributions.cards[0].widgets[0],
    { id: 'transfers', kind: 'value', label: 'Transfers', value: 4, suffix: 'connections' },
    { id: 'rate', kind: 'progress', label: 'Rate', value: 0, max: 1024 },
    { id: 'note', kind: 'value', label: 'Note', value: 'PRIVATE SAVED NOTE' },
    { id: 'ready', kind: 'value', label: 'Ready', value: false },
    { id: 'stage', kind: 'value', label: 'Stage', value: 'plan' },
  ]);
  assert.ok(Object.isFrozen(card.widgets) && card.widgets.every(Object.isFrozen));
  assert.equal(card.widgets.some(widget => Object.hasOwn(widget, 'setting')), false);
  assert.equal(m.contributions.cards[0].widgets[1].setting, 'transfers');
});

test('widgets reject duplicate IDs, unknown fields, malformed references and incompatible progress ranges', () => {
  const mutations = [
    card => card.widgets = null, card => card.widgets = {},
    card => card.widgets = Array.from({ length: 9 }, () => ({ ...card.widgets[0] })),
    card => card.widgets.push({ ...card.widgets[0] }),
    card => card.widgets[0].id = 'invalid\n', card => card.widgets[0].id = 'a'.repeat(49),
    card => card.widgets[0].kind = 'html', card => card.widgets[0].html = '<script>unsafe</script>',
    card => delete card.widgets[0].text, card => card.widgets[0].tone = 'danger',
    card => card.widgets[0].setting = 'note', card => card.widgets[0].text = ' untrimmed',
    card => card.widgets[0].text = 'x\0y', card => card.widgets[0].text = 'x\u202ey',
    card => card.widgets[0].text = '\ud800', card => card.widgets[0].text = '🦊'.repeat(1001),
    card => card.widgets[1].setting = 'undeclared', card => card.widgets[1].setting = { setting: 'rate' },
    card => card.widgets[1].label = 'multi\nline', card => card.widgets[1].label = 'x'.repeat(101),
    card => card.widgets[1].suffix = '', card => card.widgets[1].suffix = 'x'.repeat(25),
    card => card.widgets[1].suffix = 'a\tb', card => card.widgets[1].value = 3,
    card => card.widgets[2].setting = 'note', card => card.widgets[2].setting = 'missing',
    card => card.widgets[2].max = 1023, card => card.widgets[2].max = 0,
    card => card.widgets[2].max = 1_000_001, card => card.widgets[2].max = 1024.5,
    card => card.widgets[2].max = '1024', card => card.widgets[2].suffix = 'MiB/s',
  ];
  for (const mutate of mutations)
    assert.throws(() => readManifest(altered(m => mutate(m.contributions.cards[0]))), contractError);
  assert.throws(() => readManifest(altered(m => m.settings[1].min = -1)), { code: 'CONTRIBUTION_WIDGET' });
  assert.throws(() => readManifest(altered(m => m.capabilities.find(c => c.id === 'storage.settings').required = false)), { code: 'CAPABILITY_SETTINGS_REQUIRED' });
  assert.throws(() => readManifest(altered(m => m.capabilities = m.capabilities.filter(c => c.id !== 'storage.settings'))), { code: 'CAPABILITY_SETTINGS_REQUIRED' });
});

test('widget Unicode bounds count code points and accept legal maximum progress and unique local IDs', () => {
  const m = manifest(), card = m.contributions.cards[0];
  card.widgets[0].text = '🦊'.repeat(1000); card.widgets[1].label = '🦊'.repeat(100); card.widgets[1].suffix = '🦊'.repeat(24);
  card.widgets[0].id = 'a'.repeat(48);
  card.widgets.push({ id: 'another-notice', kind: 'notice', text: 'Saved', tone: 'success' },
    { id: 'warning', kind: 'notice', text: 'Review choices', tone: 'warning' });
  card.widgets[2].max = 1_000_000;
  m.settings[1].max = 1_000_000;
  card.actions = [];
  assert.equal(readManifest(source(m)).manifest.contributions.cards[0].widgets.length, 8);
});

test('download constants admit only the exact two bounded integer preferences', () => {
  assert.deepEqual(DOWNLOAD_FIELDS, { max_concurrent_transfers: { min: 1, max: 64 }, total_rate_limit_mib_per_second: { min: 0, max: 1024 } });
  assert.ok(Object.isFrozen(DOWNLOAD_FIELDS.max_concurrent_transfers));
  assert.equal(actionCapability({ kind: 'set-launcher-downloads' }), 'launcher.downloads');
  for (const values of [
    { max_concurrent_transfers: 1 }, { max_concurrent_transfers: 64 },
    { total_rate_limit_mib_per_second: 0 }, { total_rate_limit_mib_per_second: 1024 },
    { max_concurrent_transfers: 64, total_rate_limit_mib_per_second: 0 },
  ]) {
    validateDownloadsAction(values, []);
    const host = new ExtensionHost(), m = manifest(); m.contributions.cards[0].actions[0].values = values;
    const entry = installHost(host, m);
    assert.deepEqual(host.commitAction(host.prepareAction(entry.id, 'dashboard', 'apply').token),
      { kind: 'set-launcher-downloads', extensionId: entry.id, values });
  }
});

test('download actions reject every unsupported shape, preference and concrete value type', () => {
  const invalid = [null, [], {}, true, 1, 'limits',
    { max_concurrent_transfers: null }, { max_concurrent_transfers: false }, { max_concurrent_transfers: '4' },
    { max_concurrent_transfers: 0 }, { max_concurrent_transfers: 65 }, { max_concurrent_transfers: 1.5 },
    { total_rate_limit_mib_per_second: -1 }, { total_rate_limit_mib_per_second: 1025 }, { total_rate_limit_mib_per_second: 0.5 },
    { max_concurrent_transfers: 4, proxy: 'https://example.invalid' }, { theme: 'dark' }, { download_url: 'https://example.invalid' },
    { max_concurrent_transfers: 4, total_rate_limit_mib_per_second: 0, another: 1 },
    { max_concurrent_transfers: {} }, { max_concurrent_transfers: [] },
    { max_concurrent_transfers: { setting: 'transfers', value: 4 } }, { max_concurrent_transfers: { setting: 4 } },
  ];
  for (const values of invalid)
    assert.throws(() => readManifest(altered(m => m.contributions.cards[0].actions[0].values = values)), contractError);
  for (const field of ['setting', 'target', 'url'])
    assert.throws(() => readManifest(altered(m => m.contributions.cards[0].actions[0][field] = 'anything')), contractError);
  assert.throws(() => readManifest(altered(m => m.capabilities = m.capabilities.filter(c => c.id !== 'launcher.downloads'))), { code: 'CAPABILITY_UNDECLARED' });
});

test('download bindings validate their entire numeric range before any saved value is used', () => {
  const invalid = [
    m => m.settings[0].min = 0, m => m.settings[0].max = 65,
    m => m.settings[1].max = 1025,
    m => m.contributions.cards[0].actions[0].values.max_concurrent_transfers = { setting: 'note' },
    m => m.contributions.cards[0].actions[0].values.max_concurrent_transfers = { setting: 'ready' },
    m => m.contributions.cards[0].actions[0].values.max_concurrent_transfers = { setting: 'stage' },
    m => m.contributions.cards[0].actions[0].values.max_concurrent_transfers = { setting: 'unknown' },
    m => { m.settings[1].min = -1; m.contributions.cards[0].widgets = []; },
  ];
  for (const mutate of invalid) assert.throws(() => readManifest(altered(mutate)), contractError);
  const values = { max_concurrent_transfers: { setting: 'transfers' }, total_rate_limit_mib_per_second: 0 };
  validateDownloadsAction(values, manifest().settings);
  assert.deepEqual(resolveDownloads(values, { transfers: 64 }), { max_concurrent_transfers: 64, total_rate_limit_mib_per_second: 0 });
});

test('saved settings drive widgets and download intents across durable store restart and packaging', async t => {
  const { store, file, id } = await fixture(t);
  assert.equal(readPackage(createPackage(source(manifest()))).digest, store.inspect(id).digest);
  const saved = { transfers: 32, rate: 120, note: '  精确保存\n🦊 trailing  ', ready: true, stage: 'build' };
  await store.updateSettings(id, saved, expected(store.inspect(id)));
  const card = store.cards('home.banner')[0];
  assert.deepEqual(card.widgets.filter(w => w.kind !== 'notice').map(w => w.value), [32, 120, saved.note, true, 'build']);
  const reopened = await ExtensionStore.open({ file });
  assert.deepEqual(reopened.cards('home.banner')[0].widgets, card.widgets);
  assert.equal(reopened.warning, null);
  assert.deepEqual(reopened.commitAction(reopened.prepareAction(id, 'dashboard', 'apply', expected(reopened.inspect(id))).token),
    { kind: 'set-launcher-downloads', extensionId: id, values: { max_concurrent_transfers: 32, total_rate_limit_mib_per_second: 120 } });
});

test('download optional grants persist independently while required settings grants suspend widgets', async t => {
  const { store, file, id } = await fixture(t), pending = store.prepareAction(id, 'dashboard', 'apply');
  await store.revoke(id, 'launcher.downloads', expected(store.inspect(id)));
  assert.throws(() => store.commitAction(pending.token));
  const card = store.cards('home.banner')[0];
  assert.ok(card.widgets.length); assert.deepEqual(card.actions[0], { id: 'apply', label: 'Apply download limits', enabled: false, unavailableReason: 'permission-not-granted' });
  assert.throws(() => store.prepareAction(id, 'dashboard', 'apply'), { code: 'HOST_PERMISSION' });
  const restored = await ExtensionStore.open({ file });
  assert.equal(restored.cards('home.banner')[0].actions[0].enabled, false);
  const review = restored.rereview(id); await restored.confirmReview(review.token, review.capabilities.map(c => c.id));
  assert.equal(restored.cards('home.banner')[0].actions[0].enabled, true);
  await restored.revoke(id, 'storage.settings');
  assert.equal(restored.inspect(id).state, 'suspended');
  assert.deepEqual(restored.cards('home.banner'), []);
  assert.deepEqual((await ExtensionStore.open({ file })).cards('home.banner'), []);
});

test('stale snapshots, saved changes, revocation, lifecycle transitions and expiry retire pending download actions', async t => {
  const { store, id } = await fixture(t), old = store.inspect(id), action = store.prepareAction(id, 'dashboard', 'apply');
  await store.updateSettings(id, { transfers: 8 }, expected(old));
  assert.throws(() => store.prepareAction(id, 'dashboard', 'apply', expected(old)), { code: 'HOST_STALE_ENTRY' });
  assert.throws(() => store.commitAction(action.token));
  const host = new ExtensionHost(), entry = installHost(host);
  for (const change of [() => host.updateSettings(entry.id, { rate: 2 }), () => host.revoke(entry.id, 'launcher.navigate'),
    () => { host.disable(entry.id); host.enable(entry.id); }]) {
    const pending = host.prepareAction(entry.id, 'dashboard', 'apply'); change();
    assert.throws(() => host.commitAction(pending.token), { code: 'HOST_STALE_ACTION' });
  }
  const safe = host.prepareAction(entry.id, 'dashboard', 'apply'); host.setSafeMode(true); host.setSafeMode(false);
  assert.throws(() => host.commitAction(safe.token), { code: 'HOST_TOKEN' });
  const once = host.prepareAction(entry.id, 'dashboard', 'apply'); host.commitAction(once.token);
  assert.throws(() => host.commitAction(once.token), { code: 'HOST_TOKEN' });
  let now = 0; const timed = new ExtensionHost({ clock: () => now }); installHost(timed);
  const expired = timed.prepareAction(entry.id, 'dashboard', 'apply'); now = 30_000;
  assert.throws(() => timed.commitAction(expired.token), { code: 'HOST_TOKEN' });
});

test('metadata catalog excludes widget content, saved values, definitions and resolved download settings', async t => {
  const { store, id } = await fixture(t);
  const m = manifest(); m.version = '1.1.0'; m.contributions.cards[0].slot = 'rh-code.cards'; await install(store, m);
  await store.updateSettings(id, { note: 'PRIVATE WIDGET CONTENT', rate: 987 }, expected(store.inspect(id)));
  const catalog = store.catalog(), panel = catalog.panels[0], action = catalog.actions[0];
  assert.deepEqual(Object.keys(panel).sort(), ['cardId', 'digest', 'extensionId', 'extensionName', 'generation', 'title', 'viewId']);
  assert.deepEqual(Object.keys(action).sort(), ['cardId', 'cardTitle', 'digest', 'enabled', 'extensionId', 'extensionName', 'generation', 'id', 'kind', 'label', 'unavailableReason']);
  assert.equal(action.kind, 'set-launcher-downloads');
  for (const forbidden of ['PRIVATE', 'widgets', 'presentation', 'settings', 'max_concurrent_transfers', 'Line one', '987'])
    assert.equal(JSON.stringify(catalog).includes(forbidden), false);
  assert.equal(store.cards('rh-code.cards', { extensionId: id, cardId: 'dashboard' })[0].widgets[3].value, 'PRIVATE WIDGET CONTENT');
});

test('upgrades retain valid widget values and replace incompatible settings before resolving download intents', async t => {
  const { store, file, id } = await fixture(t);
  await store.updateSettings(id, { transfers: 64, rate: 400, note: 'Retained', ready: true }, expected(store.inspect(id)));
  const m = manifest(); m.version = '1.1.0'; m.settings[0].max = 32; m.settings[0].default = 8;
  m.settings[3] = { id: 'ready', label: 'Ready text', type: 'text', maxLength: 20, default: 'New default' };
  await install(store, m);
  const restored = await ExtensionStore.open({ file }), widgets = restored.cards('home.banner')[0].widgets;
  assert.deepEqual(widgets.filter(w => w.kind !== 'notice').map(w => w.value), [8, 400, 'Retained', 'New default', 'plan']);
  assert.deepEqual(restored.commitAction(restored.prepareAction(id, 'dashboard', 'apply').token).values,
    { max_concurrent_transfers: 8, total_rate_limit_mib_per_second: 400 });
});

test('failed durable writes preserve widget projections, generation and the admitted saved download values', async t => {
  const { file, id } = await fixture(t), before = await readFile(file);
  const store = await ExtensionStore.open({ file, write: async () => { throw new Error('Fixture full disk'); } });
  const entry = store.inspect(id), card = store.cards('home.banner')[0], action = store.prepareAction(id, 'dashboard', 'apply');
  await assert.rejects(store.updateSettings(id, { transfers: 64, note: 'Unpublished' }, expected(entry)), /Fixture full disk/);
  assert.deepEqual(await readFile(file), before); assert.deepEqual(store.inspect(id), entry); assert.deepEqual(store.cards('home.banner')[0], card);
  assert.equal(store.commitAction(action.token).values.max_concurrent_transfers, 4);
});

test('repeated widget values cannot publish an unreadable save and the same store remains usable after rejection', async t => {
  const root = await directory(t), file = path.join(root, 'extensions.json'), store = await ExtensionStore.open({ file });
  const entry = await install(store, amplified()), before = await readFile(file), projection = store.cards('home.quick-actions');
  await assert.rejects(store.updateSettings(entry.id, { note: '🦊'.repeat(4000) }, expected(entry)), { code: 'HOST_PROJECTION_LIMIT' });
  assert.deepEqual(await readFile(file), before); assert.deepEqual(store.inspect(entry.id), entry); assert.deepEqual(store.cards('home.quick-actions'), projection);
  await store.updateSettings(entry.id, { note: 'Readable saved value' }, expected(entry));
  const restored = await ExtensionStore.open({ file });
  assert.equal(restored.warning, null); assert.equal(restored.cards('home.quick-actions')[11].widgets[7].value, 'Readable saved value');
});

test('widget projection budgets also reject installation defaults and damaged restored amplification without overwriting bytes', async t => {
  const { store, file } = await fixture(t), before = await readFile(file), entry = store.list()[0];
  const large = amplified('🦊'.repeat(4000)); large.id = 'local.amplified';
  assert.ok(Buffer.byteLength(source(large)) < 64 * 1024);
  await assert.rejects(install(store, large), { code: 'HOST_PROJECTION_LIMIT' });
  assert.deepEqual(await readFile(file), before); assert.deepEqual(store.list(), [entry]);
  const small = amplified(); small.id = 'local.amplified'; await install(store, small);
  const saved = JSON.parse(await readFile(file, 'utf8'));
  saved.entries.find(record => record.id === small.id).settings.note = '🦊'.repeat(4000);
  await writeFile(file, JSON.stringify(saved)); const damagedBytes = await readFile(file);
  const blocked = await ExtensionStore.open({ file });
  assert.ok(blocked.warning); assert.deepEqual(blocked.list(), []);
  await assert.rejects(blocked.setSafeMode(false), { code: 'STORE_UNREADABLE' });
  assert.deepEqual(await readFile(file), damagedBytes);
});

async function isolatedHost(t, root) {
  const entry = path.join(root, 'experimental', 'host.mjs'), data = path.join(root, 'data');
  await mkdir(path.dirname(entry), { recursive: true }); await mkdir(data, { recursive: true });
  await copyFile(new URL('../../host.mjs', import.meta.url), entry);
  await cp(new URL('../src/', import.meta.url), path.join(root, 'experimental', 'extensions', 'src'), { recursive: true });
  const child = spawn(process.execPath, [entry, data], { stdio: ['pipe', 'pipe', 'pipe'] });
  const lines = createInterface({ input: child.stdout }), pending = [];
  let finished = false, stderr = '';
  child.stderr.on('data', chunk => { stderr = (stderr + chunk).slice(-4000); });
  const exited = new Promise(resolve => child.once('exit', (code, signal) => {
    finished = true;
    for (const request of pending.splice(0)) request.reject(new Error(`Isolated host exited: ${code ?? signal}; ${stderr}`));
    resolve({ code, signal });
  }));
  lines.on('line', line => pending.shift()?.resolve(JSON.parse(line)));
  t.after(async () => { if (!finished) child.kill('SIGKILL'); await exited; lines.close(); });
  async function rpc(operation, args = {}) {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('Isolated host timed out')), 5000);
      pending.push({ resolve: reply => { clearTimeout(timer); resolve(reply); }, reject: error => { clearTimeout(timer); reject(error); } });
      child.stdin.write(JSON.stringify({ operation, args }) + '\n');
    });
  }
  async function call(operation, args) { const reply = await rpc(operation, args); assert.equal(reply.ok, true, reply.error); return reply.result; }
  async function close() { assert.equal(await call('shutdown'), true); child.stdin.end(); assert.equal((await exited).code, 0, stderr); lines.close(); }
  return { rpc, call, close, data };
}

test('isolated lightweight RPC host saves, restarts and resolves widgets/downloads without Agent, native effects or vendor code', { timeout: 15000 }, async t => {
  const root = await directory(t); let host = await isolatedHost(t, root);
  assert.deepEqual((await readdir(path.join(root, 'experimental'))).sort(), ['extensions', 'host.mjs']);
  const m = manifest(), review = await host.call('extensions_review', { source: source(m) });
  let entry = await host.call('extensions_confirm', { token: review.token, grants: review.capabilities.map(c => c.id) });
  for (const slot of newSlots) assert.ok(Array.isArray(await host.call('extensions_cards', { slot })));
  entry = await host.call('extensions_settings', { id: entry.id, expected: expected(entry), values: { transfers: 16, rate: 48, note: 'RPC SAVED NOTE' } });
  const card = (await host.call('extensions_cards', { slot: 'home.banner' }))[0];
  assert.equal(card.widgets[1].value, 16); assert.equal(card.widgets[2].value, 48); assert.equal(card.widgets[3].value, 'RPC SAVED NOTE');
  const action = () => ({ extensionId: entry.id, cardId: 'dashboard', actionId: 'apply', expected: expected(entry) });
  assert.deepEqual(await host.call('extensions_action', action()), { kind: 'set-launcher-downloads', extensionId: entry.id,
    values: { max_concurrent_transfers: 16, total_rate_limit_mib_per_second: 48 } });
  assert.deepEqual(await host.call('extensions_catalog'), { panels: [], actions: [] });
  await host.close(); host = await isolatedHost(t, root);
  entry = (await host.call('extensions_list')).entries[0];
  assert.equal((await host.call('extensions_cards', { slot: 'home.banner' }))[0].widgets[3].value, 'RPC SAVED NOTE');
  assert.equal((await host.call('extensions_action', action())).values.max_concurrent_transfers, 16);
  entry = await host.call('extensions_revoke', { id: entry.id, capability: 'launcher.downloads', expected: expected(entry) });
  assert.equal((await host.rpc('extensions_action', action())).ok, false);
  assert.equal((await host.call('extensions_cards', { slot: 'home.banner' }))[0].actions[0].enabled, false);
  assert.deepEqual(await host.call('idle_prepare'), { busy: false, resumePresetId: null });
  assert.deepEqual(await readdir(host.data), ['extensions.json']);
  await host.close();
});
