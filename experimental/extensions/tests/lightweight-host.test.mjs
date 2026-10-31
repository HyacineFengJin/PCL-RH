/** Copy only the declarative host into an isolated tree. If an accidental
 * static Agent/Pi/vendor import returns, startup and plugin lifecycle fail
 * before a reply; successful RPCs prove the dependency boundary at runtime. */
import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { cp, copyFile, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const hostFile = fileURLToPath(new URL('../../host.mjs', import.meta.url));
const extensionSource = fileURLToPath(new URL('../src/', import.meta.url));
const scratch = fileURLToPath(new URL('../../../work/plugin-system-2026-10-19/', import.meta.url));
const manifest = JSON.parse(await readFile(new URL('../examples/instance-compass.json', import.meta.url), 'utf8'));
manifest.scope = 'both';
manifest.contributions.cards.push({ ...manifest.contributions.cards[0], id: 'workspace', slot: 'rh-code.cards' });
const source = JSON.stringify(manifest), id = manifest.id;
const grants = manifest.capabilities.map(capability => capability.id);
const expected = entry => ({ digest: entry.digest, generation: entry.generation });

async function fixture(t, agentSource) {
  await mkdir(scratch, { recursive: true });
  const root = await mkdtemp(path.join(scratch, 'lightweight-host-'));
  const entry = path.join(root, 'experimental', 'host.mjs'), data = path.join(root, 'data');
  await mkdir(path.dirname(entry), { recursive: true });
  await mkdir(data);
  await copyFile(hostFile, entry);
  await cp(extensionSource, path.join(root, 'experimental', 'extensions', 'src'), { recursive: true });
  if (agentSource) await writeFile(path.join(root, 'experimental', 'agent-host.mjs'), agentSource);
  t.after(() => rm(root, { recursive: true, force: true }));
  return { root, entry, data };
}

function connect(t, { entry, data }) {
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
  t.after(async () => {
    if (!finished) child.kill('SIGKILL');
    await exited; lines.close();
  });
  function rpc(operation, args = {}) {
    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error('Isolated lightweight host timed out')), 5000);
      pending.push({ resolve: result => { clearTimeout(timeout); resolve(result); },
        reject: error => { clearTimeout(timeout); reject(error); } });
      child.stdin.write(JSON.stringify({ operation, args }) + '\n');
    });
  }
  async function call(operation, args) {
    const reply = await rpc(operation, args);
    assert.equal(reply.ok, true, reply.error);
    return reply.result;
  }
  async function close() {
    assert.equal(await call('shutdown'), true);
    // The private native owner closes the transport after its shutdown reply.
    child.stdin.end();
    const exit = await exited;
    assert.equal(exit.code, 0, stderr);
    lines.close();
  }
  return { rpc, call, close };
}

test('plugin install, cards, grants, enable, remove and shutdown work without any Agent/runtime/vendor files', { timeout: 15000 }, async t => {
  const f = await fixture(t);
  assert.deepEqual((await readdir(path.dirname(f.entry))).sort(), ['extensions', 'host.mjs']);
  let host = connect(t, f);
  assert.deepEqual(await host.call('idle_prepare'), { busy: false, resumePresetId: null });
  assert.equal(await host.call('source_release', { sourceId: '00000000-0000-4000-8000-000000000001' }), false);
  assert.equal((await host.rpc('source_release', { sourceId: 'invalid' })).ok, false);
  assert.deepEqual((await readdir(path.dirname(f.entry))).sort(), ['extensions', 'host.mjs']);
  assert.equal(await host.call('idle_resume', { presetId: 'unused-native-locator' }), true);
  assert.deepEqual((await host.call('extensions_list')).entries, []);
  const cancelled = await host.call('extensions_review', { source });
  assert.deepEqual(await host.call('idle_prepare'), { busy: true });
  await host.call('extensions_cancel', { token: cancelled.token });
  assert.deepEqual(await host.call('idle_prepare'), { busy: false, resumePresetId: 'unused-native-locator' });
  const review = await host.call('extensions_review', { source });
  let entry = await host.call('extensions_confirm', { token: review.token, grants });
  assert.equal(entry.scope, 'both');
  assert.equal((await host.call('extensions_cards', { slot: 'rh-code.cards' })).length, 1);
  const context = { rootId: 'fixture', instanceId: 'fixture', revision: 'current',
    summary: { minecraftVersion: '1.21.1', loader: 'Fabric', modCount: 2, isolated: true } };
  const intent = await host.call('extensions_action', {
    extensionId: id, cardId: 'compass', actionId: 'summary', expected: expected(entry), context,
  });
  assert.equal(intent.summary.minecraftVersion, '1.21.1');
  entry = await host.call('extensions_revoke', { id, capability: 'launcher.navigate', expected: expected(entry) });
  const stale = expected(entry);
  entry = await host.call('extensions_disable', { id, expected: stale });
  assert.equal(entry.enabled, false);
  assert.equal((await host.rpc('extensions_enable', { id, expected: stale })).ok, false);
  entry = await host.call('extensions_enable', { id, expected: expected(entry) });
  assert.equal(entry.state, 'active');
  assert.equal(entry.grants.includes('launcher.navigate'), false);
  await host.call('extensions_safe_mode', { enabled: true });
  assert.deepEqual(await host.call('idle_prepare'), { busy: false, resumePresetId: 'unused-native-locator' });
  assert.equal((await host.rpc('extensions_unknown')).ok, false);
  await host.close();
  assert.deepEqual(await readdir(f.data), ['extensions.json']);

  host = connect(t, f);
  assert.equal((await host.call('extensions_list')).safeMode, true);
  await host.call('extensions_safe_mode', { enabled: false });
  entry = (await host.call('extensions_list')).entries[0];
  assert.equal(entry.grants.includes('launcher.navigate'), false);
  assert.deepEqual(await host.call('extensions_remove', { id, expected: expected(entry) }), { id, removed: true });
  assert.deepEqual((await host.call('extensions_list')).entries, []);
  assert.deepEqual(JSON.parse(await readFile(path.join(f.data, 'extensions.json'), 'utf8')).entries, []);
  await host.close();
});

test('first non-plugin RPC creates one Agent, shares the extension store and replays the deferred resume locator', { timeout: 15000 }, async t => {
  const f = await fixture(t, `
    import { appendFile } from 'node:fs/promises';
    import path from 'node:path';
    export async function createAgentHost({ root, extensions }) {
      const log = value => appendFile(path.join(root, 'agent-events.jsonl'), JSON.stringify(value) + '\\n');
      await log({ operation: 'created', installed: extensions.list().map(entry => entry.id) });
      return { async dispatch(operation, args) {
        await log({ operation, args });
        if (operation === 'status') return { entries: extensions.list() };
        if (operation === 'idle_prepare') return { busy: true, owner: 'agent' };
        if (operation === 'idle_resume' || operation === 'shutdown') return true;
        throw new Error('Unexpected fixture Agent operation');
      } };
    }
  `);
  const host = connect(t, f), log = path.join(f.data, 'agent-events.jsonl');
  await host.call('idle_resume', { presetId: 'durable-preset' });
  await host.call('extensions_list');
  const review = await host.call('extensions_review', { source });
  await host.call('extensions_confirm', { token: review.token, grants });
  await assert.rejects(readFile(log), { code: 'ENOENT' });
  assert.deepEqual((await host.call('status')).entries.map(entry => entry.id), [id]);
  await host.call('extensions_disable', { id });
  assert.equal((await host.call('status')).entries[0].state, 'disabled');
  assert.deepEqual(await host.call('idle_prepare'), { busy: true, owner: 'agent' });
  await host.close();
  const events = (await readFile(log, 'utf8')).trim().split('\n').map(line => JSON.parse(line));
  assert.equal(events.filter(event => event.operation === 'created').length, 1);
  assert.deepEqual(events.slice(0, 3).map(event => event.operation), ['created', 'idle_resume', 'status']);
  assert.equal(events[1].args.presetId, 'durable-preset');
  assert.equal(events.filter(event => event.operation === 'shutdown').length, 1);
});

test('plugin-only wakes return the unconsumed preset locator across two suspensions before Agent restoration', { timeout: 15000 }, async t => {
  const f = await fixture(t, `
    import { appendFile } from 'node:fs/promises';
    import path from 'node:path';
    export async function createAgentHost({ root }) {
      const log = value => appendFile(path.join(root, 'resume-events.jsonl'), JSON.stringify(value) + '\\n');
      let selected = null;
      await log({ operation: 'created' });
      return { async dispatch(operation, args) {
        await log({ operation, args });
        if (operation === 'idle_resume') { selected = args.presetId; return true; }
        if (operation === 'status') return { selected };
        if (operation === 'idle_prepare') return { busy: false, resumePresetId: selected };
        if (operation === 'shutdown') return true;
        throw new Error('Unexpected fixture Agent operation');
      } };
    }
  `);
  const log = path.join(f.data, 'resume-events.jsonl'), presetId = 'durable-preset';
  // Model the native owner: it keeps only the locator returned by the retiring
  // host, closes that process, then supplies the locator to the next process.
  let resumePresetId = presetId;
  for (let cycle = 0; cycle < 2; cycle++) {
    const host = connect(t, f);
    assert.equal(await host.call('idle_resume', { presetId: resumePresetId }), true);
    assert.deepEqual((await host.call('extensions_list')).entries, []);
    assert.deepEqual(await host.call('extensions_cards', { slot: 'rh-code.cards' }), []);
    await assert.rejects(readFile(log), { code: 'ENOENT' });
    const prepared = await host.call('idle_prepare');
    assert.deepEqual(prepared, { busy: false, resumePresetId: presetId });
    resumePresetId = prepared.resumePresetId;
    await host.close();
    await assert.rejects(readFile(log), { code: 'ENOENT' });
  }
  const host = connect(t, f);
  await host.call('idle_resume', { presetId: resumePresetId });
  assert.deepEqual(await host.call('status'), { selected: presetId });
  // Once created, the original Agent owns the next idle decision and locator.
  assert.deepEqual(await host.call('idle_prepare'), { busy: false, resumePresetId: presetId });
  await host.close();
  const events = (await readFile(log, 'utf8')).trim().split('\n').map(line => JSON.parse(line));
  assert.deepEqual(events.map(event => event.operation), ['created', 'idle_resume', 'status', 'idle_prepare', 'shutdown']);
  assert.equal(events[1].args.presetId, presetId);
});

test('an unavailable Agent cannot prevent subsequent plugin management or shutdown', { timeout: 10000 }, async t => {
  const f = await fixture(t), host = connect(t, f);
  assert.equal((await host.rpc('status')).ok, false);
  const review = await host.call('extensions_review', { source });
  await host.call('extensions_confirm', { token: review.token, grants: ['ui.cards'] });
  assert.equal((await host.call('extensions_list')).entries[0].state, 'active');
  assert.deepEqual(await host.call('idle_prepare'), { busy: false, resumePresetId: null });
  await host.close();
});

test('a corrupt store fails card and catalog reads without importing Agent or replacing its bytes', { timeout: 10000 }, async t => {
  const f = await fixture(t, 'throw new Error("Agent must not be imported for store recovery");');
  const file = path.join(f.data, 'extensions.json'), bytes = '{broken plugin state';
  await writeFile(file, bytes);
  const host = connect(t, f);
  for (const operation of ['extensions_cards', 'extensions_catalog']) {
    const reply = await host.rpc(operation, { slot: 'home.banner' });
    assert.equal(reply.ok, false);
    assert.match(reply.error, /Extension store unreadable; retained for recovery/);
    assert.doesNotMatch(reply.error, /Agent must not/);
  }
  const listed = await host.call('extensions_list');
  assert.deepEqual(listed.entries, []);
  assert.equal(listed.safeMode, true);
  assert.match(listed.warning, /Extension store unreadable/);
  assert.equal(await readFile(file, 'utf8'), bytes);
  await host.close();
});
