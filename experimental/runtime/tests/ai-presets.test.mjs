/** Storage/routing checks use throwaway project fixtures and fake keys only.
 * No real model requests, domain jobs or user configuration are accessed. */
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { AiPresets } from "../../ai-presets.mjs";
import {
  normalizeWorkLimits,
  PI_WORK_LIMITS,
} from "../src/providers/pi-limits.mjs";
import { LivePiProvider } from "../src/providers/pi-live.mjs";
const config = {
  provider: "pcl-custom",
  model: "offline",
  api: "openai-completions",
  baseUrl: "http://localhost:1234/v1/",
  key: "offline-not-a-real-secret",
};
const scratch = fileURLToPath(
  new URL("../../../work/ai-management-2026-10-07/fixtures/", import.meta.url),
);
async function fixture(t) {
  await fs.mkdir(scratch, { recursive: true });
  const root = await fs.mkdtemp(path.join(scratch, "presets-"));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  return {
    root,
    file: path.join(root, "ai-presets.json"),
    presets: await new AiPresets(root).init(),
  };
}
test("presets survive restart with private credentials and redacted projections", async (t) => {
  const { presets, root, file } = await fixture(t);
  const view = await presets.save({
    name: "Offline",
    config,
    limits: {
      modelTurns: 64,
      toolCalls: 180,
      wallTimeMs: 1800000,
      maximumOutputTokens: 16384,
      estimatedBudgetUsd: 12,
    },
  });
  assert.equal(JSON.stringify(view).includes(config.key), false);
  assert.equal((await fs.stat(file)).mode & 0o777, 0o600);
  const restored = await new AiPresets(root).init();
  assert.equal(restored.selected().selection.key, config.key);
  assert.equal(restored.selected().limits.maximumOutputTokens, 16384);
  assert.equal(restored.selected().selection.maxTokens, 32768);
  assert.equal(restored.view().selectedId, view.selectedId);
});
test("blank edits retain keys on the same route; a new endpoint cannot inherit them", async (t) => {
  const { presets } = await fixture(t);
  await presets.save({ name: "Offline", config });
  const id = presets.view().selectedId;
  const { key, ...withoutKey } = config;
  await presets.save({
    id,
    name: "Edited",
    config: {
      ...withoutKey,
      model: "other-offline",
      baseUrl: "http://localhost:1234/v1",
    },
  });
  assert.equal(presets.selected().selection.key, key);
  await assert.rejects(
    presets.save({
      id,
      name: "Other",
      config: { ...withoutKey, baseUrl: "http://example.invalid/v1" },
    }),
  );
  assert.equal(presets.selected().name, "Edited");
  await presets.save({
    id,
    name: "No authentication",
    config: { ...withoutKey, key: "" },
  });
  assert.equal(presets.selected().selection.key, "");
});
test("zero budgets survive save, restart and later preset edits", async (t) => {
  const { presets, root } = await fixture(t);
  const unlimited = Object.fromEntries(
    Object.keys(PI_WORK_LIMITS).map((key) => [key, 0]),
  );
  await presets.save({ name: "Unlimited", config, limits: unlimited });
  const restored = await new AiPresets(root).init();
  assert.equal(restored.view().warning, null);
  assert.deepEqual(restored.selected().limits, unlimited);
  const captured = restored.selected();
  await restored.save({
    id: captured.id,
    name: "Limited",
    config,
    limits: PI_WORK_LIMITS,
  });
  assert.deepEqual(captured.limits, unlimited);
  assert.deepEqual(restored.selected().limits, PI_WORK_LIMITS);
  assert.doesNotThrow(() => new LivePiProvider({ limits: unlimited }));
  for (const key of Object.keys(unlimited)) {
    assert.throws(() => normalizeWorkLimits({ [key]: -1 }));
    assert.throws(() => normalizeWorkLimits({ [key]: NaN }));
    assert.throws(() => normalizeWorkLimits({ [key]: Infinity }));
  }
  // Zero is a work-budget sentinel, never a way to remove transport bounds.
  for (const key of ["requestBytes", "responseBytes", "resultBytes"]) {
    assert.throws(() => new LivePiProvider({ limits: { [key]: 0 } }));
  }
});
test("job snapshots survive subsequent edits, selection and deletion", async (t) => {
  const { presets, root } = await fixture(t);
  await presets.save({ name: "First", config });
  const first = presets.selected();
  await presets.save({
    id: first.id,
    name: "Changed",
    config: { ...config, key: "different-offline-key" },
    limits: { modelTurns: 50 },
  });
  assert.equal(first.selection.key, config.key);
  assert.equal(first.limits.modelTurns, PI_WORK_LIMITS.modelTurns);
  await presets.save({ name: "Second", config });
  await presets.select(first.id);
  await presets.remove(first.id);
  assert.equal(first.name, "First");
  const restored = await new AiPresets(root).init();
  assert.equal(restored.selected().name, "Second");
});
test("invalid budgets and externally edited stores refuse without overwrite", async (t) => {
  const { presets, file } = await fixture(t);
  await presets.save({ name: "Offline", config });
  const before = await fs.readFile(file);
  await assert.rejects(
    presets.save({ name: "Invalid", config, limits: { toolCalls: 201 } }),
  );
  assert.deepEqual(await fs.readFile(file), before);
  const changed = Buffer.from('{"schemaVersion":99,"presets":[]}');
  await fs.writeFile(file, changed);
  await assert.rejects(presets.save({ name: "Other", config }));
  assert.deepEqual(await fs.readFile(file), changed);
});
test("corrupt, future, nonprivate and symlink stores are retained and blocked", async (t) => {
  const { root, file } = await fixture(t);
  for (const bytes of [
    "{malformed",
    '{"schemaVersion":2,"selectedId":null,"presets":[]}',
  ]) {
    await fs.writeFile(file, bytes, { mode: 0o600 });
    const blocked = await new AiPresets(root).init();
    assert.ok(blocked.view().warning);
    assert.equal(blocked.selected(), null);
    await assert.rejects(blocked.save({ name: "Blocked", config }));
    assert.equal(await fs.readFile(file, "utf8"), bytes);
  }
  await fs.chmod(file, 0o644);
  assert.ok((await new AiPresets(root).init()).view().warning);
  await fs.unlink(file);
  const outside = path.join(root, "retained.json");
  await fs.writeFile(outside, "retain", { mode: 0o600 });
  await fs.symlink(outside, file);
  assert.ok((await new AiPresets(root).init()).view().warning);
  assert.equal(await fs.readFile(outside, "utf8"), "retain");
});
test("configurable work limits can exceed defaults while transport bounds stay fixed", () => {
  const limits = normalizeWorkLimits({
    modelTurns: 100,
    toolCalls: 200,
    maximumOutputTokens: 32768,
    wallTimeMs: 3600000,
    estimatedBudgetUsd: 20,
  });
  assert.doesNotThrow(() => new LivePiProvider({ limits }));
  assert.throws(() => normalizeWorkLimits({ modelTurns: 1.5 }));
  assert.throws(() => normalizeWorkLimits({ requestBytes: 123 }));
  assert.throws(
    () => new LivePiProvider({ limits: { requestBytes: 2000000 } }),
  );
});
