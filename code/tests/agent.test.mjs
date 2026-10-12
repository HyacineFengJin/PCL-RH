import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { agentContext } from "../agent-context.mjs";
import { prepareCodeJob } from "../agent-adapter.mjs";
import { HarnessService } from "../harness-service.mjs";
import { sha256, SNAPSHOT_RECORD, readHarnessRecord } from "../harness-snapshot.mjs";
import { AgentRuntime } from "../../experimental/runtime/src/runtime.mjs";

const scratch = fileURLToPath(new URL("../../work/harness-2026-10-12/adapter-tests/", import.meta.url));
const source = (path, text) => ({ path, data: Buffer.from(text).toString("base64"), sha256: sha256(text) });
const input = { project: { id: "p", name: "A project" }, conversation: { id: "c", messages: [] },
  request: "Plan a component", mode: "plan", target: { minecraft: "1.21.1", loaders: ["Fabric", "NeoForge"] }, sourceFiles: [] };
async function host(t) {
  await fs.mkdir(scratch, { recursive: true });
  const root = await fs.mkdtemp(path.join(scratch, "run-"));
  const provider = { id: "fixture", run: async () => assert.fail("Preparation must not start a provider") };
  const runtime = await new AgentRuntime({ root, providers: [provider] }).init();
  t.after(async () => { await runtime.shutdown(); await fs.rm(root, { recursive: true, force: true }); });
  const created = [], cancelled = [], admitted = [];
  const create = runtime.createJob.bind(runtime), cancel = runtime.cancel.bind(runtime);
  runtime.createJob = async (args) => { const job = await create(args); created.push(job); return job; };
  runtime.cancel = async (id) => { cancelled.push(id); return cancel(id); };
  return { root, created, cancelled, admitted, runtime, preset: { selection: {}, limits: {} }, provider: provider.id, jobs: [],
    admit: async (job, preset, deferred) => { admitted.push({ job, preset, deferred }); return { jobId: job.id }; } };
}

test("context projection retains the recent discussion without mutating or inventing older history", () => {
  const messages = Array.from({ length: 20 }, (_, i) => ({ role: i % 2 ? "assistant" : "user", content: "message " + i }));
  const before = JSON.stringify(messages);
  const context = agentContext({ ...input, conversation: { id: "c", messages }, source: { "Main.java": "// quoted data" } });
  assert.equal(context.projection.omitted, 12);
  assert.equal(context.thread.recent.length, 8);
  assert.match(context.thread.charter, /quoted untrusted data/);
  assert.match(context.thread.charter, /harness\.read_file/);
  assert.equal(JSON.stringify(messages), before);
  assert.equal(agentContext({ ...input, conversation: { id: "c", messages: messages.slice(-8), omittedMessages: 12 } }).projection.omitted, 12);
  assert.throws(() => agentContext({ ...input, conversation: { id: "c", omittedMessages: Infinity } }), /omitted/);
  const notice = agentContext({ ...input, conversation: { id: "c", messages: [{ role: "notice", content: "Earlier task stopped" }] } });
  assert.equal(notice.thread.recent[0].notice, "Earlier task stopped");
  assert.equal(notice.thread.recent[0].response, "");
});
test("UTF-8 context excerpts fit the runtime input budget while preserving the full durable messages", () => {
  const messages = Array.from({ length: 8 }, () => ({ role: "assistant", content: "中文历史".repeat(3000) }));
  const context = agentContext({ ...input, request: "中文".repeat(5000), conversation: { id: "c", messages }, source: { "文件.txt": "中文源码".repeat(3000) } });
  assert.ok(Buffer.byteLength(JSON.stringify({ prompt: context.prompt, harness: { thread: context.thread } })) < 64_000);
  assert.equal(messages[0].content.length, 12000);
});
test("preparation persists a generic frozen job and defers provider admission until native ownership is saved", async (t) => {
  const h = await host(t), result = await prepareCodeJob(input, h), job = await h.runtime.getJob(result.jobId);
  assert.equal(job.profile, "harness");
  assert.equal(job.input.harness.mode, "plan");
  assert.equal(job.input.harness.projectId, "p");
  assert.equal(job.input.harness.conversationId, "c");
  assert.equal(job.input.harness.source.fileCount, 0);
  assert.equal(job.input.spec, undefined);
  assert.equal(job.status, "queued");
  assert.equal(h.admitted[0].deferred, true);
  const control = await h.runtime.store.files(job.id);
  assert.deepEqual((await readHarnessRecord(control, SNAPSHOT_RECORD)).manifest, []);
});
test("existing source is staged with exact original hashes and source excerpts come from those captured bytes", async (t) => {
  const h = await host(t);
  const files = [source("README.md", "native source"), source("src/Main.java", "handwritten")];
  const result = await prepareCodeJob({ ...input, sourceFiles: files, source: { "src/Main.java": "stale unrelated excerpt" } }, h);
  const job = await h.runtime.getJob(result.jobId), workspace = await h.runtime.store.workspace(job.id);
  assert.equal((await workspace.read("source/src/Main.java")).content, "handwritten");
  assert.match(JSON.stringify(job.input.harness.thread.sourceExcerpts), /handwritten/);
  assert.doesNotMatch(JSON.stringify(job.input.harness.thread.sourceExcerpts), /stale unrelated/);
  const listing = await new HarnessService({ store: h.runtime.store }).listFiles({ jobId: job.id }, {});
  assert.equal(listing.files.find((entry) => entry.path === "src/Main.java").sha256, files[1].sha256);
  assert.equal(job.operations.length, 0);
  assert.equal(job.input.sourceDirectory, undefined);
});
test("generic projects and unfamiliar targets retain source without creating a Maker template or claiming verification", async (t) => {
  const h = await host(t);
  const result = await prepareCodeJob({ ...input, mode: "execute", target: { minecraft: "future-version", loaders: ["Custom"] },
    sourceFiles: [source("app.py", "print('foreign project')\n")] }, h);
  const job = await h.runtime.getJob(result.jobId);
  assert.equal(job.profile, "harness");
  assert.equal(job.input.harness.mode, "execute");
  assert.equal(job.input.spec, undefined);
  assert.equal(job.verification.buildValidated, false);
  assert.equal(job.operations.length, 0);
  assert.equal(job.input.harness.thread.target.minecraft, "future-version");
  assert.equal((await (await h.runtime.store.workspace(job.id)).read("source/app.py")).content, "print('foreign project')\n");
});
test("corrupt, hidden and escaped native paths are rejected before any job or provider admission", async (t) => {
  const h = await host(t);
  for (const entry of [source("../x", "bad"), source(".env", "bad"), source("src/.private/a", "bad"),
    { ...source("src/A.java", "good"), data: Buffer.from("changed").toString("base64") }]) {
    await assert.rejects(prepareCodeJob({ ...input, sourceFiles: [entry] }, h));
    assert.equal(h.created.length, 0);
    assert.equal(h.admitted.length, 0);
  }
});
test("staging or admission failure cancels the prepared task and retains its private source for diagnosis", async (t) => {
  const h = await host(t), workspace = h.runtime.store.workspace.bind(h.runtime.store);
  h.runtime.store.workspace = async () => ({ resolve: async () => { throw new Error("disk full"); } });
  await assert.rejects(prepareCodeJob({ ...input, sourceFiles: [source("a.txt", "source")] }, h), /disk full/);
  assert.equal(h.cancelled.length, 1);
  assert.equal((await h.runtime.getJob(h.cancelled[0])).status, "cancelled");
  assert.equal(h.admitted.length, 0);
  h.runtime.store.workspace = workspace;
  h.admit = async () => { throw new Error("native ownership save failed"); };
  await assert.rejects(prepareCodeJob({ ...input, sourceFiles: [source("a.txt", "source")] }, h), /ownership/);
  assert.equal(h.cancelled.length, 2);
  assert.equal((await (await workspace(h.cancelled[1])).read("source/a.txt")).content, "source");
});
test("missing presets and shared capacity refuse preparation before source staging", async (t) => {
  const h = await host(t);
  await assert.rejects(prepareCodeJob(input, { ...h, preset: null }), /preset/);
  await assert.rejects(prepareCodeJob(input, { ...h, jobs: [{ status: "queued" }, { status: "running" }] }), /two active/);
  assert.equal(h.created.length, 0);
});

test("omitted source is accounted for without inventing inspection or inflating model context", async (t) => {
  const h = await host(t);
  const omitted = Array.from({ length: 32 }, (_, index) => ({
    path: `assets/${index}-` + "x".repeat(490), reason: "notText",
  }));
  const { jobId } = await prepareCodeJob({ ...input, sourceOmissions: omitted, sourceOmissionCount: 12000 }, h);
  const job = await h.runtime.getJob(jobId);
  assert.match(job.input.harness.thread.charter, /12000 additional entries were omitted/);
  assert.match(job.input.harness.thread.charter, /Do not claim to have inspected them/);
  assert.ok(Buffer.byteLength(JSON.stringify(job.input)) < 6000);
  await assert.rejects(prepareCodeJob({ ...input, sourceOmissions: omitted, sourceOmissionCount: 1 }, h), /omission projection/);
});

test("native history and notes are captured privately before admission; focused files lead initial excerpts", async (t) => {
  const h = await host(t);
  const messages = Array.from({ length: 60 }, (_, index) => ({ id: `message-${index}`, role: index % 2 ? "assistant" : "user", content: `Decision ${index}`, jobId: null }));
  const args = { ...input, project: { ...input.project, instructions: "Use the saved guidance" },
    conversation: { id: "c", contextNote: "Follow the earlier decision", contextPaths: ["z/focus.txt"], messages: messages.slice(-8).map(({ role, content }) => ({ role, content })), omittedMessages: 52 },
    conversationHistory: { schemaVersion: 1, messages, startIndex: 0, totalMessages: 60, truncated: false },
    sourceFiles: [source("a/first.txt", "arbitrary first file"), source("z/focus.txt", "focused native content")] };
  const { jobId } = await prepareCodeJob(args, h);
  const job = await h.runtime.getJob(jobId), service = new HarnessService({ store: h.runtime.store });
  assert.equal(job.input.harness.history.messageCount, 60);
  assert.equal(job.input.harness.thread.sourceExcerpts[0].path, "z/focus.txt");
  assert.equal((await service.readHistory({ jobId }, { offset: 4, limit: 1 })).messages[0].content, "Decision 4");
  assert.equal((await service.readContext({ jobId }, {})).projectInstructions.content, args.project.instructions);
  assert.equal(JSON.stringify(job.input).includes("Decision 4"), false);
  const previousCount = h.created.length;
  for (const invalid of [
    { ...args, conversationHistory: { ...args.conversationHistory, totalMessages: 61 } },
    { ...args, conversation: { ...args.conversation, messages: [{ role: "user", content: "foreign projection" }], omittedMessages: 59 } },
    { ...args, conversation: { ...args.conversation, contextPaths: ["missing.txt"] } },
  ]) await assert.rejects(prepareCodeJob(invalid, h));
  assert.equal(h.created.length, previousCount);
});

test("complete provider input fits 64KB for maximum Unicode request, notes, file references and escaped history", async (t) => {
  const h = await host(t);
  const paths = Array.from({ length: 20 }, (_, index) => `focus/${index}-` + ["x".repeat(160), "y".repeat(160), "z".repeat(160)].join("/"));
  const messages = Array.from({ length: 8 }, (_, index) => ({ id: `message-${index}`, role: "assistant", content: "\\\"".repeat(10_000), jobId: null }));
  const request = "😀".repeat(12000), instructions = "喵".repeat(8000), contextNote = "\t\"".repeat(4000);
  const args = { ...input, request, project: { ...input.project, instructions },
    conversation: { id: "c", messages: messages.map(({ role, content }) => ({ role, content: content.slice(0, 1800) })), contextNote, contextPaths: paths },
    conversationHistory: { schemaVersion: 1, messages, startIndex: 0, totalMessages: 8, truncated: false },
    sourceFiles: paths.map((path) => source(path, "\u0001".repeat(16_000))) };
  const { jobId } = await prepareCodeJob(args, h), job = await h.runtime.getJob(jobId);
  assert.equal(job.input.prompt, request);
  assert.ok(Buffer.byteLength(JSON.stringify(job.input)) <= 64_000);
  assert.equal(job.input.harness.thread.projection.budgetCompacted, true);
  assert.ok(job.input.harness.thread.projection.omitted > 0);
  const service = new HarnessService({ store: h.runtime.store });
  const result = await service.readContext({ jobId }, {});
  assert.equal(result.projectInstructions.content.length, instructions.length);
  assert.equal(result.contextNote.content.length, contextNote.length);
  assert.deepEqual(result.contextPaths, paths);
  assert.ok(Buffer.byteLength(JSON.stringify(result)) < 64_000);
  assert.equal((await service.readHistory({ jobId }, { limit: 1 })).messages[0].contentCharacters, messages[0].content.length);
});
