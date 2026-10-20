import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { ExtensionStore } from '../src/store.mjs';
import { panelViewId } from '../src/host.mjs';

const examples = Object.fromEntries(await Promise.all(['workspace-notes', 'workspace-shortcuts', 'appearance-switcher'].map(async name =>
  [name, await readFile(new URL(`../examples/${name}.json`, import.meta.url), 'utf8')])));
const expected = entry => ({ digest: entry.digest, generation: entry.generation });
async function fixture(t, names = ['workspace-notes', 'workspace-shortcuts', 'appearance-switcher']) {
  const scratch = fileURLToPath(new URL('../../../work/plugin-panels-2026-10-20/', import.meta.url));
  await mkdir(scratch, { recursive: true });
  const root = await mkdtemp(path.join(scratch, 'catalog-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const file = path.join(root, 'extensions.json'), store = await ExtensionStore.open({ file });
  for (const name of names) await install(store, examples[name]);
  return { store, file };
}
async function install(store, source) {
  const review = store.prepareReview(source);
  return store.confirmReview(review.token, review.capabilities.map(capability => capability.id));
}

test('the catalog publishes scoped identities and operations without saved text or launcher-only cards', async t => {
  const { store } = await fixture(t);
  const notes = 'PRIVATE SAVED NOTE';
  await store.updateSettings('local.workspace-notes', { note: notes }, expected(store.inspect('local.workspace-notes')));
  const catalog = store.catalog();
  assert.equal(catalog.panels.length, 2);
  assert.equal(catalog.actions.length, 4);
  assert.ok(catalog.panels.every(panel => panel.viewId.startsWith('rh.extensions.panel.') && panel.viewId.length <= 120));
  assert.ok(catalog.actions.every(action => action.cardId !== 'home'));
  assert.equal(JSON.stringify(catalog).includes('appearance-switcher'), false);
  assert.equal(JSON.stringify(catalog).includes(notes), false);
  assert.ok(Object.isFrozen(catalog.panels[0]));
  assert.ok(catalog.actions.every(action => typeof action.digest === 'string' && Number.isSafeInteger(action.generation)));
});

test('panel identity is stable across settings, upgrades, restart and reinstall, with unambiguous ID pairs', async t => {
  const { file, store } = await fixture(t, ['workspace-notes']);
  const original = store.catalog().panels[0];
  assert.equal(original.viewId, 'rh.extensions.panel.' + createHash('sha256').update(JSON.stringify([original.extensionId, original.cardId])).digest('hex'));
  assert.notEqual(panelViewId('a.b', 'c'), panelViewId('a', 'b.c'));
  await store.updateSettings(original.extensionId, { note: 'Saved' }, expected(store.inspect(original.extensionId)));
  assert.equal(store.catalog().panels[0].viewId, original.viewId);
  const upgraded = JSON.parse(examples['workspace-notes']); upgraded.version = '1.2.0'; upgraded.contributions.cards[0].title = 'Renamed notes';
  await install(store, JSON.stringify(upgraded));
  let reopened = await ExtensionStore.open({ file });
  assert.equal(reopened.catalog().panels[0].viewId, original.viewId);
  assert.equal(reopened.catalog().panels[0].title, 'Renamed notes');
  await reopened.remove(original.extensionId);
  await install(reopened, examples['workspace-notes']);
  reopened = await ExtensionStore.open({ file });
  assert.equal(reopened.catalog().panels[0].viewId, original.viewId);
});

test('filtered card reads select one exact pair, retain scope checks and cannot mutate state', async t => {
  const { store, file } = await fixture(t), before = await readFile(file);
  const filter = { extensionId: 'local.workspace-notes', cardId: 'notes' };
  assert.equal(store.cards('rh-code.cards', filter).length, 1);
  assert.equal(store.cards('rh-code.cards', filter)[0].extensionId, filter.extensionId);
  assert.deepEqual(store.cards('home.secondary', filter), []);
  assert.deepEqual(store.cards('rh-code.cards', { ...filter, cardId: 'unknown' }), []);
  for (const invalid of [null, [], {}, { extensionId: filter.extensionId }, { ...filter, path: 'ignored' }, { ...filter, cardId: '' }])
    assert.throws(() => store.cards('rh-code.cards', invalid), { code: 'HOST_FILTER' });
  assert.deepEqual(await readFile(file), before);
});

test('disable, required grant revocation, safe mode and removal withdraw panels while optional grants disable actions', async t => {
  const { store } = await fixture(t, ['workspace-notes']);
  const id = 'local.workspace-notes', viewId = store.catalog().panels[0].viewId;
  await store.revoke(id, 'clipboard.write');
  assert.equal(store.catalog().panels[0].viewId, viewId);
  assert.equal(store.catalog().actions[0].enabled, false);
  assert.equal(store.catalog().actions[0].unavailableReason, 'permission-not-granted');
  await store.disable(id);
  assert.deepEqual(store.catalog(), { panels: [], actions: [] });
  await store.enable(id);
  assert.equal(store.catalog().panels[0].viewId, viewId);
  await store.revoke(id, 'ui.cards');
  assert.deepEqual(store.catalog(), { panels: [], actions: [] });
  const review = store.rereview(id);
  await store.confirmReview(review.token, review.capabilities.map(capability => capability.id));
  await store.setSafeMode(true);
  assert.deepEqual(store.catalog(), { panels: [], actions: [] });
  await store.setSafeMode(false);
  assert.equal(store.catalog().panels[0].viewId, viewId);
  await store.remove(id);
  assert.deepEqual(store.catalog(), { panels: [], actions: [] });
});

test('summary catalog availability follows the current host projection without leaking target bindings', async t => {
  const { store } = await fixture(t, ['workspace-shortcuts']);
  let summary = store.catalog().actions.find(action => action.id === 'summary');
  assert.equal(summary.enabled, false);
  assert.equal(summary.unavailableReason, 'no-selected-instance');
  store.setContext({ rootId: 'private-root', instanceId: 'private-instance', revision: 'current',
    summary: { minecraftVersion: '1.21.1', loader: 'Fabric', modCount: 4, isolated: true } });
  summary = store.catalog().actions.find(action => action.id === 'summary');
  assert.equal(summary.enabled, true);
  assert.equal(JSON.stringify(store.catalog()).includes('private-root'), false);
  store.setContext(null);
  assert.equal(store.catalog().actions.find(action => action.id === 'summary').enabled, false);
});

test('catalog actions use the installed saved action and reject stale receipts after edits or revocation', async t => {
  const { store } = await fixture(t, ['workspace-notes']);
  const old = store.catalog().actions[0], id = old.extensionId;
  await store.updateSettings(id, { note: 'Current saved note' }, expected(store.inspect(id)));
  assert.throws(() => store.prepareAction(id, old.cardId, old.id, expected(old)), { code: 'HOST_STALE_ENTRY' });
  const current = store.catalog().actions[0];
  const action = store.prepareAction(id, current.cardId, current.id, expected(current));
  assert.equal(store.commitAction(action.token).text, 'Current saved note');
  await store.revoke(id, 'clipboard.write');
  assert.throws(() => store.prepareAction(id, current.cardId, current.id, expected(current)), { code: 'HOST_STALE_ENTRY' });
});

test('an over-budget catalog cannot publish an unreadable installation even when slot cards fit', async t => {
  const { store, file } = await fixture(t, []);
  const original = JSON.parse(examples['workspace-shortcuts']);
  let accepted = 0, rejected = false;
  for (let index = 0; index < 32; index++) {
    const manifest = structuredClone(original);
    manifest.id = `catalog.example-${index}`; manifest.scope = 'rh-code'; manifest.name = '🦊'.repeat(80);
    manifest.contributions.cards = Array.from({ length: 12 }, (_, n) => ({
      id: `card-${n}`, slot: 'rh-code.cards', title: '🦊'.repeat(100), text: '.',
      actions: Array.from({ length: 4 }, (_, a) => ({ id: `action-${a}`, label: '🦊'.repeat(64), kind: 'navigate', target: 'settings' })),
    }));
    const before = accepted ? await readFile(file) : null;
    try { await install(store, JSON.stringify(manifest)); accepted++; }
    catch (error) {
      assert.equal(error.code, 'HOST_PROJECTION_LIMIT'); rejected = true;
      assert.deepEqual(await readFile(file), before);
      assert.ok(Buffer.byteLength(JSON.stringify(store.cards('rh-code.cards'))) < 768 * 1024);
      assert.ok(Buffer.byteLength(JSON.stringify(store.catalog())) < 768 * 1024);
      break;
    }
  }
  assert.ok(accepted > 0 && rejected);
  assert.equal((await ExtensionStore.open({ file })).catalog().panels.length, accepted * 12);
});
