import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readFile, writeFile, readdir, symlink, rm } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { ExtensionStore } from '../src/store.mjs';
import { ExtensionHost } from '../src/host.mjs';
import { readManifest } from '../src/manifest.mjs';
import { canonicalJson } from '../src/json.mjs';
import { createPackage } from '../src/package.mjs';

const source = await readFile(new URL('../examples/instance-compass.json', import.meta.url), 'utf8');
const original = JSON.parse(source), id = original.id;
const grants = original.capabilities.map(capability => capability.id);
const scratch = fileURLToPath(new URL('../../../work/plugin-system-2026-10-19/', import.meta.url));
const expected = entry => ({ digest: entry.digest, generation: entry.generation });
async function fixture(t) {
  await mkdir(scratch, { recursive: true });
  const root = await mkdtemp(path.join(scratch, 'extension-store-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  return { root, file: path.join(root, 'extensions.json') };
}
async function installed(t) {
  const f = await fixture(t), store = await ExtensionStore.open(f);
  await store.confirmReview(store.prepareReview(source).token, grants);
  return { ...f, store };
}
function scoped(scope, slots) {
  const manifest = structuredClone(original);
  if (scope !== undefined) manifest.scope = scope;
  manifest.contributions.cards = slots.map((slot, index) => ({ ...manifest.contributions.cards[0], id: `card-${index}`, slot }));
  return JSON.stringify(manifest);
}

test('scope defaults preserve the old approved digest and restrict every card slot', () => {
  const loaded = readManifest(source);
  assert.equal(Object.hasOwn(loaded.manifest, 'scope'), false);
  assert.equal(loaded.digest, createHash('sha256').update(canonicalJson(original)).digest('hex'));
  for (const [scope, slots] of [
    [undefined, ['home.secondary', 'tools.cards']], ['launcher', ['tools.cards']],
    ['rh-code', ['rh-code.cards']], ['both', ['home.secondary', 'rh-code.cards']],
  ]) {
    const host = new ExtensionHost(), review = host.prepareReview(scoped(scope, slots));
    assert.equal(review.scope, scope ?? 'launcher');
    assert.deepEqual(review.slots, slots.slice().sort());
    const entry = host.confirmReview(review.token, grants);
    assert.equal(entry.scope, scope ?? 'launcher');
    assert.equal(entry.enabled, true);
    assert.deepEqual(entry.slots, slots.slice().sort());
    for (const slot of slots) assert.equal(host.cards(slot).length, 1);
  }
  for (const [scope, slots] of [[undefined, ['rh-code.cards']], ['launcher', ['rh-code.cards']], ['rh-code', ['tools.cards']]])
    assert.throws(() => readManifest(scoped(scope, slots)), { code: 'CONTRIBUTION_SCOPE' });
  for (const scope of ['invalid', null, false, 0])
    assert.throws(() => readManifest(scoped(scope, ['tools.cards'])), { code: 'MANIFEST_SCOPE' });
});

test('installed grants, direct enable/disable and safe mode survive independent reopen', async t => {
  const { file, store } = await installed(t);
  await store.revoke(id, 'launcher.navigate', expected(store.inspect(id)));
  await store.disable(id, expected(store.inspect(id)));
  let reopened = await ExtensionStore.open({ file });
  assert.equal(reopened.inspect(id).enabled, false);
  assert.equal(reopened.inspect(id).grants.includes('launcher.navigate'), false);
  assert.deepEqual(reopened.cards('tools.cards'), []);
  await reopened.enable(id, expected(reopened.inspect(id)));
  assert.equal(reopened.inspect(id).state, 'active');
  assert.equal(reopened.cards('tools.cards')[0].actions.find(action => action.id === 'instances').enabled, false);
  await reopened.setSafeMode(true);
  reopened = await ExtensionStore.open({ file });
  assert.equal(reopened.safeMode, true);
  assert.equal(reopened.inspect(id).state, 'safe-mode');
  assert.deepEqual(reopened.cards('tools.cards'), []);
  await reopened.setSafeMode(false);
  assert.equal((await ExtensionStore.open({ file })).inspect(id).state, 'active');
});

test('required-grant revocation remains suspended after enable; removal erases source and grants', async t => {
  const { file, store } = await installed(t);
  await store.revoke(id, 'ui.cards');
  await store.disable(id);
  await store.enable(id);
  assert.equal(store.inspect(id).state, 'suspended');
  assert.equal((await ExtensionStore.open({ file })).inspect(id).state, 'suspended');
  const review = store.rereview(id);
  assert.equal(store.pendingReviewCount, 1);
  await store.remove(id, expected(store.inspect(id)));
  assert.equal(store.pendingReviewCount, 0);
  assert.deepEqual(store.list(), []);
  const saved = JSON.parse(await readFile(file, 'utf8'));
  assert.deepEqual(saved.entries, []);
  assert.equal(JSON.stringify(saved).includes(id), false);
  await assert.rejects(store.confirmReview(review.token, grants), { code: 'HOST_TOKEN' });
  const fresh = store.prepareReview(source);
  assert.equal(fresh.mode, 'install');
  assert.ok(fresh.capabilities.every(capability => !capability.currentlyGranted));
});

test('every failed durable mutation retains the published entry and original file', async t => {
  const { file } = await installed(t);
  const bytes = await readFile(file);
  const store = await ExtensionStore.open({ file, write: async () => { throw new Error('Fixture disk failure'); } });
  const before = store.list();
  for (const mutate of [
    () => store.disable(id), () => store.enable(id), () => store.revoke(id, 'ui.cards'),
    () => store.remove(id), () => store.setSafeMode(true),
  ]) {
    await assert.rejects(mutate(), /Fixture disk failure/);
    assert.deepEqual(store.list(), before);
    assert.equal(store.safeMode, false);
    assert.deepEqual(await readFile(file), bytes);
  }
  const updated = JSON.parse(source); updated.version = '1.1.0'; updated.name = 'Updated';
  const review = store.prepareReview(JSON.stringify(updated));
  await assert.rejects(store.confirmReview(review.token, grants), /Fixture disk failure/);
  assert.deepEqual(store.list(), before);
  assert.deepEqual(await readFile(file), bytes);
  assert.equal(store.pendingReviewCount, 0);
  await assert.rejects(store.confirmReview(review.token, grants), { code: 'HOST_TOKEN' });
  assert.equal(store.cards('tools.cards')[0].extensionName, original.name);
});

test('failed first installation consumes consent without installing anything', async t => {
  const { file, root } = await fixture(t);
  const store = await ExtensionStore.open({ file, write: async () => { throw new Error('Fixture disk failure'); } });
  const review = store.prepareReview(source);
  await assert.rejects(store.confirmReview(review.token, grants), /Fixture disk failure/);
  assert.deepEqual(store.list(), []);
  assert.equal(store.pendingReviewCount, 0);
  assert.deepEqual(await readdir(root), []);
});

test('serialized mutation admission rejects a stale entry after the preceding commit', async t => {
  const { file } = await installed(t);
  let release, started;
  const entered = new Promise(resolve => { started = resolve; });
  const gate = new Promise(resolve => { release = resolve; });
  let writes = 0;
  const store = await ExtensionStore.open({ file, write: async (target, bytes) => {
    writes++; started(); await gate; await writeFile(target, bytes);
  } });
  const receipt = expected(store.inspect(id));
  const disable = store.disable(id, receipt), remove = store.remove(id, receipt);
  await entered;
  assert.equal(store.inspect(id).state, 'active');
  assert.throws(() => store.prepareReview(source), { code: 'STORE_BUSY' });
  assert.throws(() => store.prepareAction(id, 'compass', 'instances'), { code: 'STORE_BUSY' });
  release(); await disable;
  await assert.rejects(remove, { code: 'HOST_STALE_ENTRY' });
  assert.equal(writes, 1);
  assert.equal(store.inspect(id).state, 'disabled');
});

test('publication keeps the latest context and retires old actions without reviving cancelled consent', async t => {
  const { file } = await installed(t);
  let release, started;
  const entered = new Promise(resolve => { started = resolve; });
  const gate = new Promise(resolve => { release = resolve; });
  const store = await ExtensionStore.open({ file, write: async (target, bytes) => {
    started(); await gate; await writeFile(target, bytes);
  } });
  const context = version => ({ rootId: 'root', instanceId: 'instance', revision: version,
    summary: { minecraftVersion: version, loader: 'Fabric', modCount: 1, isolated: true } });
  store.setContext(context('1.20.1'));
  const action = store.prepareAction(id, 'compass', 'summary');
  const review = store.rereview(id);
  const pending = store.revoke(id, 'launcher.navigate');
  await entered;
  store.setContext(context('1.21.1'));
  store.cancelReview(review.token);
  assert.throws(() => store.commitAction(action.token), { code: 'STORE_BUSY' });
  release(); await pending;
  assert.equal(store.pendingReviewCount, 0);
  assert.throws(() => store.commitAction(action.token));
  const next = store.prepareAction(id, 'compass', 'summary');
  assert.equal(store.commitAction(next.token).summary.minecraftVersion, '1.21.1');
  await assert.rejects(store.confirmReview(review.token, grants), { code: 'HOST_TOKEN' });
});

test('a fixed stale .new file cannot block atomic saves; external store edits are preserved', async t => {
  const { file, root, store } = await installed(t);
  const stale = path.join(root, 'extensions.json.new');
  await writeFile(stale, 'retained stale temporary data');
  await store.disable(id);
  assert.equal(await readFile(stale, 'utf8'), 'retained stale temporary data');
  const before = store.list();
  await writeFile(file, 'external replacement');
  await assert.rejects(store.enable(id), { code: 'STORE_CHANGED' });
  assert.deepEqual(store.list(), before);
  assert.equal(await readFile(file, 'utf8'), 'external replacement');
  assert.deepEqual((await readdir(root)).sort(), ['extensions.json', 'extensions.json.new']);
});

test('legacy stores restore missing safeMode and malformed stores are retained without grants', async t => {
  const { file } = await installed(t);
  const document = JSON.parse(await readFile(file, 'utf8'));
  delete document.safeMode;
  await writeFile(file, JSON.stringify(document));
  const legacy = await ExtensionStore.open({ file });
  assert.equal(legacy.safeMode, false);
  assert.equal(legacy.inspect(id).state, 'active');
  for (const invalid of [
    { ...document, entries: [...document.entries, ...document.entries] },
    { ...document, entries: [{ ...document.entries[0], grants: ['ui.cards', 'ui.cards'] }] },
    { ...document, entries: [{ ...document.entries[0], digest: '0'.repeat(64) }] },
    { ...document, safeMode: 'false' }, { ...document, schemaVersion: 2 },
  ]) {
    const text = JSON.stringify(invalid); await writeFile(file, text);
    const store = await ExtensionStore.open({ file });
    assert.ok(store.warning); assert.equal(store.safeMode, true); assert.deepEqual(store.list(), []);
    assert.throws(() => store.prepareReview(source), { code: 'STORE_UNREADABLE' });
    await assert.rejects(store.setSafeMode(false), { code: 'STORE_UNREADABLE' });
    assert.equal(await readFile(file, 'utf8'), text);
  }
});

test('expired reviews stop pinning the host and cannot be confirmed', async t => {
  const { file } = await fixture(t);
  let now = 100;
  const store = await ExtensionStore.open({ file, clock: () => now });
  const review = store.prepareReview(source);
  assert.equal(store.pendingReviewCount, 1);
  now = review.expiresAt;
  assert.equal(store.pendingReviewCount, 0);
  await assert.rejects(store.confirmReview(review.token, grants), { code: 'HOST_TOKEN' });
});

test('packaged RH Code declarations retain exact source, scope and integrity across reopen', async t => {
  const { file } = await fixture(t);
  const store = await ExtensionStore.open({ file });
  const source = createPackage(scoped('rh-code', ['rh-code.cards']));
  const review = store.preparePackageReview(source);
  assert.equal(review.scope, 'rh-code');
  await store.confirmReview(review.token, ['ui.cards']);
  const reopened = await ExtensionStore.open({ file });
  assert.equal(reopened.inspect(id).package.digest, review.package.digest);
  assert.equal(reopened.inspect(id).scope, 'rh-code');
  assert.equal(reopened.cards('rh-code.cards').length, 1);
  assert.equal(reopened.cards('tools.cards').length, 0);
  assert.equal(reopened.rereview(id).digest, review.digest);
  assert.equal(JSON.parse(await readFile(file, 'utf8')).entries[0].source, source);
});

test('a linked store is rejected and never overwrites its target', async t => {
  const { file, root } = await fixture(t);
  const target = path.join(root, 'retained.json'), bytes = 'retained linked content';
  await writeFile(target, bytes);
  try { await symlink(target, file, 'file'); }
  catch (error) {
    if (['EPERM', 'ENOSYS', 'ENOTSUP'].includes(error.code)) { t.skip('Platform does not permit creating a test symlink'); return; }
    throw error;
  }
  const store = await ExtensionStore.open({ file });
  assert.ok(store.warning); assert.equal(store.safeMode, true);
  await assert.rejects(store.setSafeMode(false), { code: 'STORE_UNREADABLE' });
  assert.equal(await readFile(target, 'utf8'), bytes);
});
