/** Isolated host build contracts: fake helper/fixtures only. No user project,
 * model transport, dependency download or authored native compiler is invoked. */
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";
import { JobStore } from "../../experimental/runtime/src/store.mjs";
import { AgentRuntime } from "../../experimental/runtime/src/runtime.mjs";
import { ToolRegistry } from "../../experimental/runtime/src/tools.mjs";
import { registerBuildTools } from "../../experimental/runtime/src/build-tools.mjs";
import { registerHarnessTools } from "../../experimental/runtime/src/harness-tools.mjs";
import { HarnessToolCatalog } from "../harness-tool-catalog.mjs";
import { prepareCodeJob } from "../agent-adapter.mjs";
import { HarnessService } from "../harness-service.mjs";
import { HarnessBuildService } from "../build-service.mjs";
import { NativeBuildRunner } from "../build-runner.mjs";
import { captureBuildInput, stageBuildInput, captureBuildSource, BUILD_LIMITS } from "../build-snapshot.mjs";
import { captureNativeSnapshot, stageNativeSnapshot, sha256 } from "../harness-snapshot.mjs";

const scratch = fileURLToPath(new URL("../../work/harness-runner-2026-10-13/node-tests/", import.meta.url));
const entry = (path, bytes) => ({ path, size: Buffer.byteLength(bytes), sha256: sha256(bytes), data: Buffer.from(bytes).toString("base64") });
const binary = (path, bytes) => ({ path, size: bytes.length, sha256: sha256(bytes), base64: bytes.toString("base64") });
async function root(t) {
  await fs.mkdir(scratch, { recursive: true });
  const directory = await fs.mkdtemp(path.join(scratch, "run-"));
  t.after(() => fs.rm(directory, { recursive: true, force: true }));
  return directory;
}
function fakeRunner({ text = "compiler output 中文🙂\n", status = "completed", artifact = true } = {}) {
  return { calls: [], hasActiveWork: () => false, shutdown: async () => {},
    async run(request, { signal, onFrame }) {
      this.calls.push(request);
      assert.equal(signal.aborted, false);
      const files = await fs.readdir(path.join(request.root, "source"));
      assert.ok(files.includes("src"));
      await onFrame({ type: "started" });
      await onFrame({ type: "output", stream: "stderr", text });
      if (artifact) {
        const directory = path.join(request.root, "source/.rh-build/classes");
        await fs.mkdir(directory, { recursive: true });
        await fs.writeFile(path.join(directory, "Main.class"), Buffer.from([0xca, 0xfe, 0xba, 0xbe]));
      }
      return { status, exitCode: status === "completed" ? 0 : 1, signal: null, error: null };
    } };
}
async function fixture(t, { mode = "execute", enabled = true, network = false, files = [entry("src/Main.java", "class Main {}\n")],
  buildFiles = [], runner = fakeRunner() } = {}) {
  const directory = await root(t), store = await new JobStore(directory).init();
  const source = captureNativeSnapshot(files), build = captureBuildInput({ enabled, network }, buildFiles, source.record.manifest);
  const job = await store.create({ profile: "harness", provider: "isolated", status: "running",
    input: { harness: { schemaVersion: 1, mode, pluginId: "rh.project", projectId: "project", conversationId: "conversation",
      source: source.descriptor, buildPolicy: build.policy, buildInput: build.descriptor } },
    operations: [{ id: "plan", name: "harness.report_plan", status: "completed", result: { steps: ["Compile"] } }] });
  await stageNativeSnapshot(store, job, source); await stageBuildInput(store, job, build);
  const harness = new HarnessService({ store }), builds = new HarnessBuildService({ store, harnessService: harness, runner });
  t.after(() => builds.shutdown());
  return { directory, store, job, harness, builds, runner, ctx: { jobId: job.id, operationId: "compile-1", signal: new AbortController().signal } };
}
const args = { recipe: "javac", target: "classes" };

test("native build policy and binary inventory fail before task creation when authority or paths are malformed", () => {
  assert.deepEqual(captureBuildInput(undefined).policy, { enabled: false, network: false });
  for (const policy of [{ enabled: true }, { enabled: "true", network: false }, { enabled: true, network: false, shell: true }])
    assert.throws(() => captureBuildInput(policy), { code: "INVALID_BUILD_POLICY" });
  for (const name of ["README.png", ".gradle/a", "src/../a", "src/.secret/a", "src/build/a", "src", "common/src"])
    assert.throws(() => captureBuildInput(undefined, [binary(name, Buffer.from([0]))]));
  const supported = captureBuildInput({ enabled: true, network: false }, [binary("common/src/main/resources/a.png", Buffer.from([0])),
    binary("gradle/wrapper/gradle-wrapper.jar", Buffer.from([1]))]);
  assert.equal(supported.descriptor.fileCount, 2);
  assert.throws(() => captureBuildInput(undefined, [binary("src/a.bin", Buffer.alloc(1_048_577))]), { code: "INVALID_BUILD_INPUT" });
  assert.throws(() => captureBuildInput(undefined, [binary("src/a", Buffer.from([0]))], [{ path: "src/a" }]), { code: "INVALID_BUILD_INPUT" });
});

test("Ask, Plan and captured disabled builds cannot run even when a build tool is registered", async (t) => {
  for (const options of [{ mode: "ask" }, { mode: "plan" }, { enabled: false }]) {
    const f = await fixture(t, options);
    await assert.rejects(f.builds.run(f.ctx, args), { code: "BUILD_NOT_AUTHORIZED" });
    assert.equal(f.runner.calls.length, 0);
  }
});

test("captured policy cannot be widened by editing job input after admission", async (t) => {
  const f = await fixture(t, { enabled: false });
  const job = await f.store.load(f.job.id); job.input.harness.buildPolicy.enabled = true; await f.store.save(job);
  await assert.rejects(f.builds.run(f.ctx, args), { code: "BUILD_INPUT_CHANGED" });
  assert.equal(f.runner.calls.length, 0);
});

test("build tool schema rejects commands, shell, paths, network overrides and partial proposal identity", async (t) => {
  const f = await fixture(t), registry = registerBuildTools(new ToolRegistry(), f.builds), tool = registry.get("build.run", ["build.run"]);
  for (const extra of [{ command: "echo unsafe" }, { shell: true }, { cwd: "/" }, { network: true }, { timeoutMs: 1 },
    { proposalId: "a".repeat(32) }, { expectedDigest: "a".repeat(64) }])
    assert.throws(() => tool.validate({ ...args, ...extra }), { code: "INVALID_ARGUMENTS" });
  assert.equal(tool.replaySafety, "manual");
});

test("a private build runs under the existing job lease, copies binary inputs and freezes network authority", async (t) => {
  const f = await fixture(t, { buildFiles: [binary("src/main/resources/a.bin", Buffer.from([0, 255]))], network: true });
  const release = await f.store.acquire(f.job.id);
  try {
    const result = await f.builds.run(f.ctx, args);
    assert.equal(result.status, "completed"); assert.equal(result.actualGeneratedCodeExecuted, true);
    assert.equal(result.artifactCount, 1); assert.match(result.logTail, /中文🙂/);
    assert.equal(f.runner.calls[0].network, true);
    assert.equal(f.runner.calls[0].timeoutMs, 600_000);
    assert.notEqual(f.runner.calls[0].root, path.join(f.directory, f.job.id, "workspace/source"));
    assert.deepEqual(await fs.readFile(path.join(f.runner.calls[0].root, "source/src/main/resources/a.bin")), Buffer.from([0, 255]));
    const artifact = await f.builds.artifactDirectory(f.job.id, result.runId);
    assert.deepEqual(await fs.readFile(path.join(artifact.directory, result.artifacts[0].path)), Buffer.from([0xca, 0xfe, 0xba, 0xbe]));
    assert.equal((await f.store.load(f.job.id)).operations.length, 1, "service never writes runtime-owned job metadata");
  } finally { await release(); }
});

test("running jobs can compile an exact proposal overlay while source adoption retains its completed-job gate", async (t) => {
  const f = await fixture(t);
  const proposal = await f.harness.proposeChanges({ ...f.ctx, operationId: "proposal" }, { summary: "Change Main",
    changes: [{ path: "src/Main.java", expected_sha256: sha256("class Main {}\n"), content: "class Main { int n; }\n" }] });
  const job = await f.store.load(f.job.id); job.operations.push({ id: "proposal", name: "harness.propose_changes", status: "completed", result: proposal }); await f.store.save(job);
  const overlay = { ...args, proposalId: proposal.proposalId, expectedDigest: proposal.digest };
  await assert.rejects(f.harness.exportProposal(f.job.id, overlay), { code: "PROPOSAL_NOT_READY" });
  await assert.rejects(f.builds.run(f.ctx, { ...overlay, expectedDigest: "a".repeat(64) }), { code: "BUILD_PROPOSAL_CHANGED" });
  const result = await f.builds.run(f.ctx, overlay);
  assert.equal(result.proposalId, proposal.proposalId);
  assert.equal(await fs.readFile(path.join(f.runner.calls[0].root, "source/src/Main.java"), "utf8"), "class Main { int n; }\n");
  assert.equal(await fs.readFile(path.join(f.directory, f.job.id, "workspace/source/src/Main.java"), "utf8"), "class Main {}\n");
});

test("proposal overlay cannot replace a frozen binary asset", async (t) => {
  const f = await fixture(t, { buildFiles: [binary("src/a.bin", Buffer.from([0]))] });
  const proposal = await f.harness.proposeChanges({ ...f.ctx, operationId: "proposal" }, { summary: "Unsafe binary overwrite",
    changes: [{ path: "src/a.bin", expected_sha256: null, content: "text" }] });
  const job = await f.store.load(f.job.id); job.operations.push({ id: "proposal", name: "harness.propose_changes", status: "completed", result: proposal }); await f.store.save(job);
  await assert.rejects(captureBuildSource(f.harness, f.ctx, { proposalId: proposal.proposalId, expectedDigest: proposal.digest }), { code: "BUILD_INPUT_CONFLICT" });
});

test("operation replay returns the existing build receipt without running the helper again", async (t) => {
  const f = await fixture(t), first = await f.builds.run(f.ctx, args), second = await f.builds.run(f.ctx, args);
  assert.equal(first.runId, second.runId); assert.equal(f.runner.calls.length, 1);
  await assert.rejects(f.builds.run(f.ctx, { ...args, recipe: "gradle" }), { code: "BUILD_REQUEST_CONFLICT" });
  const registry = registerBuildTools(new ToolRegistry(), f.builds), receipt = registry.get("build.run", ["build.run"]).projectReceipt(first);
  assert.equal(receipt.logTail, undefined); assert.equal(receipt.artifacts, undefined);
});

test("lost build ownership is projected as interrupted and a reserved operation is never automatically rerun", async (t) => {
  const f = await fixture(t), result = await f.builds.run(f.ctx, args);
  const control = await f.store.files(f.job.id), recordPath = `control/build-runs/${result.runId}.json`;
  const record = JSON.parse((await control.read(recordPath)).content); record.status = "running"; record.finishedAt = null;
  await control.write(recordPath, JSON.stringify(record));
  const view = await f.builds.listForJob(await f.store.load(f.job.id));
  assert.equal(view[0].status, "interrupted"); assert.equal(view[0].error.code, "BUILD_OWNER_LOST");
  await assert.rejects(f.builds.run(f.ctx, args), { code: "BUILD_INTERRUPTED" });
  assert.equal(f.runner.calls.length, 1);
});

test("log byte pagination preserves split UTF-8 and bounds receipt metadata", async (t) => {
  const text = "🙂中文\n".repeat(8), f = await fixture(t, { runner: fakeRunner({ text }) });
  const result = await f.builds.run(f.ctx, args); let offset = 0, reconstructed = "";
  do {
    const page = await f.builds.readLog(f.ctx, { runId: result.runId, offset, limit: 5 });
    assert.equal(page.totalBytes, Buffer.byteLength(text)); assert.doesNotMatch(page.content, /�/);
    reconstructed += page.content; offset = page.nextOffset;
  } while (offset !== null);
  assert.equal(reconstructed, text);
  const inside = await f.builds.readLog(f.ctx, { runId: result.runId, offset: 1, limit: 5 });
  assert.doesNotMatch(inside.content, /�/);
  const other = await fixture(t);
  await assert.rejects(other.builds.readLog(other.ctx, { runId: result.runId }));
});

test("unsafe compiler artifacts and altered frozen source never receive a completed build receipt", async (t) => {
  const f = await fixture(t), run = f.runner.run.bind(f.runner);
  f.runner.run = async (...parameters) => {
    const result = await run(...parameters);
    const directory = path.join(parameters[0].root, "source/.rh-build/classes");
    await fs.symlink(path.join(f.directory, f.job.id, "workspace/source/src/Main.java"), path.join(directory, "Leak.class"));
    return result;
  };
  const result = await f.builds.run(f.ctx, args);
  assert.equal(result.status, "failed"); assert.equal(result.error.code, "BUILD_ARTIFACT_UNSAFE");
  const g = await fixture(t), runG = g.runner.run.bind(g.runner);
  g.runner.run = async (...parameters) => {
    const outcome = await runG(...parameters);
    await fs.writeFile(path.join(g.directory, g.job.id, "workspace/source/src/Main.java"), "changed");
    return outcome;
  };
  assert.equal((await g.builds.run(g.ctx, args)).status, "failed");
});

test("binary input changes fail admission and collected artifacts are verified again before exposing their folder", async (t) => {
  const f = await fixture(t, { buildFiles: [binary("src/a.bin", Buffer.from([0, 255]))] });
  await fs.writeFile(path.join(f.directory, f.job.id, "build-input/src/a.bin"), Buffer.from([1, 255]));
  await assert.rejects(f.builds.run(f.ctx, args), { code: "SOURCE_SNAPSHOT_CHANGED" });
  assert.equal(f.runner.calls.length, 0);
  const g = await fixture(t), result = await g.builds.run(g.ctx, args);
  const artifact = await g.builds.artifactDirectory(g.job.id, result.runId);
  assert.equal(result.artifacts[0].path, "classes/Main.class");
  await fs.writeFile(path.join(artifact.directory, result.artifacts[0].path), Buffer.from([0, 0, 0, 0]));
  await assert.rejects(g.builds.artifactDirectory(g.job.id, result.runId), { code: "BUILD_ARTIFACT_CHANGED" });
});

test("escaped compiler controls are paged within the model result budget and output limits remain explicit", async (t) => {
  const f = await fixture(t, { runner: fakeRunner({ text: "\0".repeat(20_000), status: "output_limit", artifact: false }) });
  const result = await f.builds.run(f.ctx, args);
  assert.equal(result.status, "output_limit"); assert.equal(result.logTruncated, true);
  const page = await f.builds.readLog(f.ctx, { runId: result.runId, limit: 16_000 });
  assert.ok(Buffer.byteLength(JSON.stringify(page)) < 64_000);
  assert.equal(page.totalBytes, 20_000); assert.ok(page.nextOffset > 0 && page.nextOffset < 16_000);
  const denied = new NativeBuildRunner({ executable: path.join(f.directory, "not-installed-build-helper") });
  const blocked = await denied.run({ schemaVersion: 1, runId: randomUUID(), root: f.directory, ...args,
    network: false, timeoutMs: BUILD_LIMITS.timeoutMs, maxOutputBytes: BUILD_LIMITS.outputBytes });
  assert.equal(blocked.status, "blocked"); await denied.shutdown();
});

test("build shutdown cancels the in-flight helper and waits for cleanup before releasing ownership", async (t) => {
  let ready, drained = false;
  const started = new Promise((resolve) => { ready = resolve; });
  const runner = { calls: [], hasActiveWork: () => !drained && runner.calls.length > 0, shutdown: async () => {},
    async run(request, { signal, onFrame }) {
      this.calls.push(request); await onFrame({ type: "started" }); ready();
      await new Promise((resolve) => signal.addEventListener("abort", resolve, { once: true }));
      await new Promise((resolve) => setTimeout(resolve, 20)); drained = true;
      return { status: "cancelled", exitCode: null, signal: "SIGTERM", error: null };
    } };
  const f = await fixture(t, { runner }), pending = f.builds.run(f.ctx, args);
  await started; assert.equal(f.builds.hasActiveWork(), true);
  await f.builds.shutdown();
  assert.equal(drained, true); assert.equal(f.builds.hasActiveWork(), false);
  const result = await pending; assert.equal(result.status, "cancelled");
  assert.equal((await f.builds.listForJob(f.job))[0].status, "cancelled");
});

test("runtime build tools use the captured plugin scope, keep compact receipts and allow later log reads", async (t) => {
  const directory = await root(t), registry = new ToolRegistry(), catalog = new HarnessToolCatalog(registry), runner = fakeRunner();
  let builds;
  const provider = { id: "isolated", async run(ctx) {
    await ctx.invoke("plan", "harness.report_plan", { summary: "Compile", steps: ["Compile private source"] });
    const build = await ctx.invoke("compile", "build.run", args);
    assert.equal(build.status, "completed");
    assert.match((await ctx.invoke("log", "build.read_log", { runId: build.runId })).content, /compiler/);
    return { text: "Compiler command completed" };
  } };
  const runtime = await new AgentRuntime({ root: directory, tools: registry, providers: [provider] }).init();
  const harness = new HarnessService({ store: runtime.store });
  builds = new HarnessBuildService({ store: runtime.store, harnessService: harness, runner });
  registerHarnessTools(catalog, harness); registerBuildTools(catalog.forPlugin("rh.build"), builds);
  t.after(async () => { await runtime.shutdown(); await builds.shutdown(); catalog.dispose(); });
  const { jobId } = await prepareCodeJob({ project: { id: "p", name: "Private fixture" }, conversation: { id: "c", messages: [] },
    request: "Compile", mode: "execute", sourceFiles: [entry("src/Main.java", "class Main {}")], buildPolicy: { enabled: true, network: false } },
  { runtime, preset: { selection: {}, limits: {} }, provider: "isolated", jobs: [], admit: async (job) => ({ jobId: job.id }) });
  const completed = await runtime.start(jobId);
  assert.equal(completed.status, "completed"); assert.equal(runner.calls.length, 1);
  const receipt = completed.operations.find((operation) => operation.name === "build.run").result;
  assert.equal(receipt.logTail, undefined); assert.equal(receipt.artifacts, undefined);
  assert.equal((await builds.listForJob(completed))[0].artifactCount, 1);
});

const helperSource = `import fs from 'node:fs/promises';
import readline from 'node:readline';
const mode = process.argv[2], lines = readline.createInterface({input:process.stdin});
let request, timer;
const event = (value) => process.stdout.write(JSON.stringify({schemaVersion:1,runId:request.runId,...value})+'\\n');
const finish = (status, code=status==='completed'?0:1) => { event({type:'finished',status,exitCode:status==='completed'?0:null,signal:null,error:null}); lines.close(); process.stdin.destroy(); clearInterval(timer); process.exitCode=code; };
lines.on('line', line => {
 if (!request) {
  request = JSON.parse(line);
  if (process.env.BUILD_FIXTURE_SECRET) { finish('failed'); return; }
  if (mode==='blocked') { finish('blocked'); return; }
  event({type:'started'});
  if (mode==='malformed') { process.stdout.write('invalid JSON\\n'); timer=setInterval(()=>{},1000); return; }
  if (mode==='crash') { process.exit(2); }
  if (mode==='hold' || mode==='stubborn') { timer=setInterval(()=>{},1000); return; }
  event({type:'output',stream:'stdout',text:'中文🙂 fixture output\\n'}); finish('completed');
 } else if (mode !== 'stubborn' && JSON.parse(line).type==='cancel') finish('cancelled');
});
`;
async function nativeFixture(t, mode, cancelGraceMs = 300) {
  const directory = await root(t), script = path.join(directory, "helper.mjs"); await fs.writeFile(script, helperSource);
  const runner = new NativeBuildRunner({ executable: process.execPath, args: [script, mode], cancelGraceMs });
  t.after(() => runner.shutdown());
  return { runner, request: { schemaVersion: 1, runId: randomUUID(), root: directory, recipe: "javac", target: "classes",
    network: false, timeoutMs: BUILD_LIMITS.timeoutMs, maxOutputBytes: BUILD_LIMITS.outputBytes } };
}
test("native helper NDJSON accepts completion and blocked failure exit codes without leaking ambient environment", async (t) => {
  const previous = process.env.BUILD_FIXTURE_SECRET; process.env.BUILD_FIXTURE_SECRET = "private synthetic value";
  t.after(() => { if (previous === undefined) delete process.env.BUILD_FIXTURE_SECRET; else process.env.BUILD_FIXTURE_SECRET = previous; });
  const success = await nativeFixture(t, "complete"), frames = [];
  assert.equal((await success.runner.run(success.request, { onFrame: async (frame) => frames.push(frame) })).status, "completed");
  assert.match(frames.find((frame) => frame.type === "output").text, /中文🙂/);
  const blocked = await nativeFixture(t, "blocked");
  assert.equal((await blocked.runner.run(blocked.request)).status, "blocked");
});
test("native helper cancellation drains its final receipt and shutdown clears process ownership", async (t) => {
  const f = await nativeFixture(t, "hold"), controller = new AbortController();
  const pending = f.runner.run(f.request, { signal: controller.signal, onFrame: async (frame) => {
    if (frame.type === "started") controller.abort();
  } });
  assert.equal(f.runner.hasActiveWork(), true);
  assert.equal((await pending).status, "cancelled"); assert.equal(f.runner.hasActiveWork(), false);
});
test("native malformed/crashed/stubborn helpers cannot produce a success receipt", async (t) => {
  const malformed = await nativeFixture(t, "malformed");
  assert.equal((await malformed.runner.run(malformed.request)).status, "interrupted");
  const crash = await nativeFixture(t, "crash");
  assert.equal((await crash.runner.run(crash.request)).status, "interrupted");
  const stubborn = await nativeFixture(t, "stubborn", 50), controller = new AbortController();
  const result = await stubborn.runner.run(stubborn.request, { signal: controller.signal, onFrame: async (frame) => {
    if (frame.type === "started") controller.abort();
  } });
  assert.equal(result.status, "interrupted"); assert.equal(result.error.code, "BUILD_CLEANUP_UNCONFIRMED");
  assert.equal(stubborn.runner.hasActiveWork(), false);
});
