/** Isolated bundled-plugin scopes, registration retirement and actual runtime
 * dispatch. No source files, model endpoint, native app or credentials run. */
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { HarnessToolCatalog, captureHarnessToolScope } from "../harness-tool-catalog.mjs";
import { ToolRegistry } from "../../experimental/runtime/src/tools.mjs";
import { AgentRuntime } from "../../experimental/runtime/src/runtime.mjs";

const allModes = ["ask", "plan", "execute"];
const manifests = () => [
  { id: "rh.project", version: "1", capabilities: [], dependencies: [], tools: [] },
  { id: "rh.files", version: "1", capabilities: ["project.files.read"], dependencies: ["rh.project"], tools: [
    { name: "harness.read_file", capability: "project.files.read", modes: allModes },
  ] },
  { id: "rh.notes", version: "1", capabilities: ["project.notes.read"], dependencies: ["rh.project"], tools: [
    { name: "notes.inspect", capability: "project.notes.read", modes: allModes },
  ] },
  { id: "rh.changes", version: "1", capabilities: ["project.review"], dependencies: ["rh.files"], tools: [
    { name: "harness.propose_changes", capability: "project.review", modes: ["execute"] },
  ] },
];
const implementation = (definition, execute = async () => ({ inspected: true })) => ({
  name: definition.name, owner: definition.owner, capability: definition.capability, modes: [...definition.modes],
  description: "Inspect an isolated bundled project note", inputSchema: { type: "object", properties: {}, additionalProperties: false },
  replaySafety: "read-only", validate: args => assert.deepEqual(args, {}), execute,
});
const job = (scope, mode = "ask") => ({ profile: "harness", input: { harness: { mode, ...scope } } });
function fixture(definitions = manifests()) {
  const registry = new ToolRegistry(), catalog = new HarnessToolCatalog(registry, definitions);
  const scope = captureHarnessToolScope(definitions.map(owner => owner.id), "execute", definitions);
  for (const grant of scope.toolGrants) catalog.forPlugin(grant.owner).register(implementation(grant));
  return { registry, catalog, definitions };
}

test("captured owner scopes exclude disabled plugins and enforce mode/dependency declarations", () => {
  const { registry, catalog, definitions } = fixture();
  const scope = captureHarnessToolScope(["rh.project", "rh.files"], "ask", definitions);
  assert.deepEqual(catalog.resolve(job(scope)), ["harness.read_file"]);
  assert.deepEqual(registry.describe(catalog.resolve(job(scope))).map(tool => tool.name), ["harness.read_file"]);
  assert.throws(() => registry.get("notes.inspect", catalog.resolve(job(scope))), { code: "TOOL_DENIED" });
  assert.throws(() => captureHarnessToolScope(["rh.files"], "ask", definitions), { code: "INVALID_TOOL_SCOPE" });
  assert.throws(() => captureHarnessToolScope(["unknown"], "ask", definitions), { code: "INVALID_TOOL_SCOPE" });
  assert.throws(() => captureHarnessToolScope(["rh.project", "rh.project"], "ask", definitions), { code: "INVALID_TOOL_SCOPE" });
  assert.equal(captureHarnessToolScope(undefined, "plan", definitions).toolGrants.some(tool => tool.name === "harness.propose_changes"), false);
  catalog.dispose();
});

test("foreign tools, undeclared capabilities and widened modes never register", () => {
  const { registry, catalog, definitions } = fixture();
  const grant = captureHarnessToolScope(undefined, "ask", definitions).toolGrants.find(tool => tool.name === "notes.inspect");
  for (const change of [{ owner: "rh.files" }, { capability: "project.files.write" }, { modes: ["ask"] }, { name: "notes.undeclared" }])
    assert.throws(() => catalog.register({ ...implementation(grant), ...change }), { code: "TOOL_OWNERSHIP_DENIED" });
  assert.throws(() => catalog.forPlugin("rh.files").register(implementation(grant)), { code: "TOOL_OWNERSHIP_DENIED" });
  const broken = manifests();
  broken[2].tools[0].capability = "project.files.write";
  assert.throws(() => new HarnessToolCatalog(new ToolRegistry(), broken), { code: "INVALID_TOOL_MANIFEST" });
  assert.equal(registry.describe(["notes.inspect"]).length, 1);
  catalog.dispose();
});

test("retirement revokes retained handlers and an old release cannot remove a replacement", async () => {
  const { registry, catalog, definitions } = fixture();
  const grant = captureHarnessToolScope(undefined, "ask", definitions).toolGrants.find(tool => tool.name === "notes.inspect");
  const retained = registry.get("notes.inspect", ["notes.inspect"]);
  const staleRegistrar = catalog.forPlugin("rh.notes");
  assert.deepEqual(await retained.execute({}, {}), { inspected: true });
  catalog.retire("rh.notes");
  assert.throws(() => staleRegistrar.register(implementation(grant)), { code: "TOOL_UNAVAILABLE" });
  assert.throws(() => retained.execute({}, {}), { code: "TOOL_UNAVAILABLE" });
  assert.throws(() => registry.get("notes.inspect", ["notes.inspect"]), { code: "TOOL_UNAVAILABLE" });
  catalog.forPlugin("rh.notes").register(implementation(grant, async () => ({ replacement: true })));
  assert.throws(() => retained.execute({}, {}), { code: "TOOL_UNAVAILABLE" });
  assert.deepEqual(await registry.get("notes.inspect", ["notes.inspect"]).execute({}, {}), { replacement: true });
  const raw = new ToolRegistry(), release = raw.contribute(implementation(grant));
  release(); raw.register(implementation(grant)); release();
  assert.equal(raw.describe(["notes.inspect"]).length, 1);
  catalog.dispose();
});

test("captured grants do not widen after a host adds tools and version changes reject resume", () => {
  const before = manifests(), scope = captureHarnessToolScope(undefined, "ask", before);
  const later = manifests();
  later[2].tools.push({ name: "notes.extra", capability: "project.notes.read", modes: allModes });
  const { catalog } = fixture(later);
  assert.deepEqual(catalog.resolve(job(scope)), ["harness.read_file", "notes.inspect"]);
  assert.deepEqual(catalog.resolve(job({}, "execute")), ["harness.read_file", "harness.propose_changes"], "legacy jobs receive no new plugin tools");
  const forged = structuredClone(scope); forged.toolGrants[0].owner = "rh.notes";
  assert.throws(() => catalog.resolve(job(forged)), { code: "TOOL_SCOPE_CHANGED" });
  const missingOwner = structuredClone(scope); delete missingOwner.enabledPlugins;
  assert.throws(() => catalog.resolve(job(missingOwner)), { code: "INVALID_TOOL_SCOPE" });
  catalog.dispose();
  const changed = manifests(); changed[2].version = "2";
  const next = fixture(changed);
  assert.throws(() => next.catalog.resolve(job(scope)), { code: "TOOL_SCOPE_CHANGED" });
  next.catalog.dispose();
});

test("registration copies the original handler before plugin descriptor mutation", async () => {
  const registry = new ToolRegistry();
  const spec = implementation({ name: "notes.inspect", owner: "rh.notes", capability: "project.notes.read", modes: allModes });
  registry.register(spec);
  spec.name = "notes.changed"; spec.execute = async () => ({ replaced: true });
  assert.deepEqual(await registry.get("notes.inspect", ["notes.inspect"]).execute({}, {}), { inspected: true });
});

test("a bundled plugin adds an actual Harness tool without editing the profile whitelist", async t => {
  const scratch = fileURLToPath(new URL("../../work/harness-context-2026-10-12/catalog/", import.meta.url));
  await fs.mkdir(scratch, { recursive: true });
  const root = await fs.mkdtemp(path.join(scratch, "run-"));
  const { registry, catalog, definitions } = fixture();
  let effect = 0;
  catalog.retire("rh.notes");
  const grant = captureHarnessToolScope(undefined, "ask", definitions).toolGrants.find(tool => tool.name === "notes.inspect");
  catalog.forPlugin("rh.notes").register(implementation(grant, async () => ({ inspected: ++effect })));
  const provider = { id: "isolated", run: async ctx => {
    assert.equal(ctx.toolDescriptions.some(tool => tool.name === "notes.inspect"), true);
    assert.deepEqual(await ctx.invoke("read-note", "notes.inspect", {}), { inspected: 1 });
    catalog.retire("rh.notes");
    await assert.rejects(ctx.invoke("retired-note", "notes.inspect", {}), { code: "TOOL_UNAVAILABLE" });
    return { text: "Read completed before plugin retirement" };
  } };
  const runtime = await new AgentRuntime({ root, tools: registry, providers: [provider] }).init();
  t.after(async () => { await runtime.shutdown(); catalog.dispose(); await fs.rm(root, { recursive: true, force: true }); });
  const scoped = captureHarnessToolScope(undefined, "ask", definitions);
  const prepared = await runtime.createJob({ profile: "harness", provider: "isolated", input: job(scoped).input });
  const completed = await runtime.start(prepared.id);
  assert.equal(completed.status, "completed");
  assert.equal(effect, 1);
  assert.equal(completed.operations.length, 1);
});
