import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { readManifest } from '../src/manifest.mjs';
import { ExtensionStore } from '../src/store.mjs';
import { ExtensionHost } from '../src/host.mjs';
import { createPackage, readPackage } from '../src/package.mjs';

const files = ['appearance-switcher', 'launch-checklist', 'workspace-notes'];
const examples = Object.fromEntries(await Promise.all(files.map(async name =>
  [name, await readFile(new URL(`../examples/${name}.json`, import.meta.url), 'utf8')])));
const receipt = entry => ({ digest: entry.digest, generation: entry.generation });
async function fixture(t, name = 'appearance-switcher') {
  const scratch = fileURLToPath(new URL('../../../work/plugin-capabilities-2026-10-20/', import.meta.url));
  await mkdir(scratch, { recursive: true });
  const root = await mkdtemp(path.join(scratch, 'settings-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const file = path.join(root, 'extensions.json'), store = await ExtensionStore.open({ file });
  const review = store.prepareReview(examples[name]);
  const entry = await store.confirmReview(review.token, review.capabilities.map(capability => capability.id));
  return { file, root, store, id: entry.id };
}
function alter(name, mutate) {
  const manifest = JSON.parse(examples[name]); mutate(manifest);
  return JSON.stringify(manifest);
}

test('all distributable examples validate, package and provide their actual declared controls', () => {
  const host = new ExtensionHost();
  for (const name of files) {
    const parsed = readManifest(examples[name]);
    assert.equal(readPackage(createPackage(examples[name])).digest, parsed.digest);
    const review = host.prepareReview(examples[name]);
    host.confirmReview(review.token, review.capabilities.map(capability => capability.id));
  }
  const cards = ['home.secondary', 'settings.cards', 'rh-code.cards'].flatMap(slot => host.cards(slot));
  assert.equal(cards.length, 4);
  assert.deepEqual(new Set(cards.flatMap(card => card.fields.map(field => field.type))), new Set(['select', 'toggle', 'number', 'text']));
  assert.ok(cards.every(card => Object.isFrozen(card.fields)));
  assert.deepEqual(host.cards('settings.cards')[0].fields.map(field => field.value), ['system', true]);
});

test('invalid schemas, controls, bindings and missing permission cannot be installed', () => {
  const cases = [
    ['appearance-switcher', m => m.settings.push(m.settings[0])],
    ['appearance-switcher', m => m.settings[0].options[0].value = 'uncontrolled-theme'],
    ['appearance-switcher', m => m.settings[1].default = 'true'],
    ['appearance-switcher', m => m.settings[0].default = 'unknown'],
    ['appearance-switcher', m => m.contributions.cards[0].fields.push('unknown')],
    ['appearance-switcher', m => m.contributions.cards[0].fields.push('theme')],
    ['appearance-switcher', m => m.capabilities.find(c => c.id === 'storage.settings').required = false],
    ['appearance-switcher', m => m.contributions.cards[0].actions[0].values.global_font = 'arbitrary'],
    ['appearance-switcher', m => m.contributions.cards[0].actions[0].values.theme = { setting: 'cursor' }],
    ['appearance-switcher', m => m.contributions.cards[0].actions[0].values.theme = null],
    ['workspace-notes', m => m.contributions.cards[0].actions[0].setting = 'stage'],
    ['workspace-notes', m => m.settings[0].default = '\0hidden'],
    ['workspace-notes', m => m.settings[0].maxLength = 4001],
    ['launch-checklist', m => m.settings[2].min = 361],
    ['launch-checklist', m => m.settings[2].default = 30.5],
  ];
  for (const [name, mutate] of cases) assert.throws(() => readManifest(alter(name, mutate)), { name: 'ContractError' });
});

test('settings persist independently and shared cards reflect only committed saved choices', async t => {
  const { file, store, id } = await fixture(t);
  const original = store.inspect(id);
  const next = await store.updateSettings(id, { theme: 'dark', cursor: false }, receipt(original));
  assert.ok(next.generation > original.generation);
  assert.deepEqual(store.cards('home.secondary')[0].fields.map(field => field.value), ['dark', false]);
  assert.deepEqual(store.cards('settings.cards')[0].fields.map(field => field.value), ['dark', false]);
  const intent = store.commitAction(store.prepareAction(id, 'appearance', 'apply', receipt(next)).token);
  assert.deepEqual(intent.values, { theme: 'dark', custom_cursor: false });
  const reopened = await ExtensionStore.open({ file });
  assert.deepEqual(reopened.cards('home.secondary')[0].fields.map(field => field.value), ['dark', false]);
  // A renderer cannot supply different appearance values as action arguments.
  const action = reopened.prepareAction(id, 'appearance', 'apply', receipt(reopened.inspect(id)));
  assert.deepEqual(reopened.commitAction(action.token).values, intent.values);
});

test('bad, stale and absent setting snapshots leave disk and visible values unchanged', async t => {
  const { file, store, id } = await fixture(t, 'launch-checklist');
  const before = await readFile(file), entry = store.inspect(id);
  for (const values of [{}, { fps: 29 }, { fps: 361 }, { fps: 60.5 }, { backup: 1 }, { undeclared: false }, null]) {
    await assert.rejects(store.updateSettings(id, values, receipt(entry)));
    assert.deepEqual(await readFile(file), before);
  }
  await assert.rejects(store.updateSettings(id, { fps: 60 }), { code: 'HOST_STALE_ENTRY' });
  const next = await store.updateSettings(id, { fps: 60 }, receipt(entry));
  await assert.rejects(store.updateSettings(id, { fps: 90 }, receipt(entry)), { code: 'HOST_STALE_ENTRY' });
  assert.equal(store.inspect(id).generation, next.generation);
});

test('failed persistence publishes neither new settings nor new action values', async t => {
  const { file, id } = await fixture(t);
  const before = await readFile(file);
  const store = await ExtensionStore.open({ file, write: async () => { throw new Error('Fixture full disk'); } });
  const entry = store.inspect(id);
  await assert.rejects(store.updateSettings(id, { theme: 'dark' }, receipt(entry)), /Fixture full disk/);
  assert.deepEqual(store.inspect(id), entry);
  assert.deepEqual(await readFile(file), before);
  assert.equal(store.commitAction(store.prepareAction(id, 'appearance', 'apply').token).values.theme, 'system');
});

test('saving settings retires old actions; disable, revoke and safe mode refuse saves', async t => {
  const { store, id } = await fixture(t);
  const action = store.prepareAction(id, 'appearance', 'apply');
  await store.updateSettings(id, { theme: 'dark' }, receipt(store.inspect(id)));
  assert.throws(() => store.commitAction(action.token));
  await store.disable(id);
  await assert.rejects(store.updateSettings(id, { theme: 'light' }, receipt(store.inspect(id))), { code: 'HOST_INACTIVE' });
  await store.enable(id); await store.revoke(id, 'storage.settings');
  await assert.rejects(store.updateSettings(id, { theme: 'light' }, receipt(store.inspect(id))), { code: 'HOST_INACTIVE' });
  const review = store.rereview(id);
  await store.confirmReview(review.token, review.capabilities.map(c => c.id));
  await store.setSafeMode(true);
  await assert.rejects(store.updateSettings(id, { theme: 'light' }, receipt(store.inspect(id))), { code: 'HOST_INACTIVE' });
});

test('upgrades retain compatible values, default changed types and never migrate another plugin state', async t => {
  const { file, store, id } = await fixture(t, 'workspace-notes');
  await store.updateSettings(id, { note: 'Saved note', stage: 'build' }, receipt(store.inspect(id)));
  const source = alter('workspace-notes', m => {
    m.version = '1.2.0'; m.settings[1] = { id: 'stage', label: 'Completed', type: 'toggle', default: false };
  });
  const review = store.prepareReview(source);
  await store.confirmReview(review.token, review.capabilities.map(c => c.id));
  const fields = (await ExtensionStore.open({ file })).cards('rh-code.cards')[0].fields;
  assert.equal(fields.find(field => field.id === 'note').value, 'Saved note');
  assert.equal(fields.find(field => field.id === 'stage').value, false);
  await store.remove(id);
  assert.equal((await readFile(file, 'utf8')).includes('Saved note'), false);
  const fresh = store.prepareReview(examples['workspace-notes']);
  await store.confirmReview(fresh.token, fresh.capabilities.map(c => c.id));
  assert.equal(store.cards('rh-code.cards')[0].fields.find(field => field.id === 'note').value, '');
});

test('copy-text uses saved Unicode text with its exact whitespace and respects independent grants', async t => {
  const { file, store, id } = await fixture(t, 'workspace-notes');
  const note = '  笔记\n🦊 trailing  ';
  await store.updateSettings(id, { note }, receipt(store.inspect(id)));
  assert.equal(store.commitAction(store.prepareAction(id, 'notes', 'copy').token).text, note);
  await store.revoke(id, 'clipboard.write');
  assert.equal(store.cards('rh-code.cards')[0].actions[0].enabled, false);
  assert.throws(() => store.prepareAction(id, 'notes', 'copy'), { code: 'HOST_PERMISSION' });
  const restored = await ExtensionStore.open({ file });
  assert.equal(restored.cards('rh-code.cards')[0].fields.find(field => field.id === 'note').value, note);
});

test('invalid on-disk setting values retain the full damaged store without loading grants', async t => {
  const { file } = await fixture(t);
  const saved = JSON.parse(await readFile(file, 'utf8'));
  saved.entries[0].settings.theme = 'uncontrolled';
  await writeFile(file, JSON.stringify(saved));
  const bytes = await readFile(file), restored = await ExtensionStore.open({ file });
  assert.ok(restored.warning); assert.deepEqual(restored.list(), []);
  await assert.rejects(restored.setSafeMode(false), { code: 'STORE_UNREADABLE' });
  assert.deepEqual(await readFile(file), bytes);
});

test('repeated field projections cannot commit a state that exceeds the readable card budget', async t => {
  const { file, store, id } = await fixture(t, 'workspace-notes');
  const source = alter('workspace-notes', m => {
    m.version = '1.2.0';
    m.settings = Array.from({ length: 7 }, (_, index) => ({
      id: `note-${index}`, label: 'Note', type: 'text', default: '', maxLength: 4000,
    }));
    m.contributions.cards = Array.from({ length: 12 }, (_, index) => ({
      id: `notes-${index}`, slot: 'rh-code.cards', title: 'Notes', text: 'Text',
      fields: m.settings.map(field => field.id), actions: [],
    }));
  });
  const review = store.prepareReview(source);
  await store.confirmReview(review.token, review.capabilities.map(capability => capability.id));
  const before = await readFile(file), entry = store.inspect(id);
  const values = Object.fromEntries(Array.from({ length: 7 }, (_, index) => [`note-${index}`, '🦊'.repeat(4000)]));
  // The input fits an ordinary request, but repeated field projections used
  // to exceed the one-megabyte reply only after a successful save.
  assert.ok(Buffer.byteLength(JSON.stringify(values)) < 128000);
  await assert.rejects(store.updateSettings(id, values, receipt(entry)), { code: 'HOST_PROJECTION_LIMIT' });
  assert.deepEqual(await readFile(file), before);
  assert.deepEqual(store.inspect(id), entry);
  assert.ok(store.cards('rh-code.cards').every(card => card.fields.every(field => field.value === '')));
  const reopened = await ExtensionStore.open({ file });
  assert.equal(reopened.warning, null);
  assert.equal(reopened.cards('rh-code.cards').length, 12);
});
