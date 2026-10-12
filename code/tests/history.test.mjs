import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { JobStore } from "../../experimental/runtime/src/store.mjs";
import { captureNativeSnapshot, stageNativeSnapshot, sha256, readHarnessRecord } from "../harness-snapshot.mjs";
import { captureNativeHistory, stageNativeHistory, HISTORY_RECORD, encodedBytes } from "../history-snapshot.mjs";
import { HarnessService } from "../harness-service.mjs";
import { ToolRegistry } from "../../experimental/runtime/src/tools.mjs";
import { registerHarnessTools } from "../../experimental/runtime/src/harness-tools.mjs";

const scratch = fileURLToPath(new URL("../../work/harness-2026-10-12/history-tests/", import.meta.url));
const message = (index, content = `history ${index}`) => ({ id: `message-${index}`, role: index % 2 ? "assistant" : "user", content, jobId: index % 2 ? `job-${index}` : null });
const envelope = (messages, startIndex = 0) => ({ schemaVersion: 1, messages, startIndex, totalMessages: startIndex + messages.length, truncated: startIndex > 0 });
const capture = (history, options = {}, manifest = []) => captureNativeHistory({ project: { id: "project", instructions: options.instructions ?? "" },
  conversation: { id: "conversation", messages: options.recent ?? [], omittedMessages: history?.totalMessages ?? 0,
    contextNote: options.contextNote ?? "", contextPaths: options.contextPaths ?? [] }, ...(history ? { conversationHistory: history } : {}) }, manifest);
async function fixture(t, history, options = {}) {
  await fs.mkdir(scratch, { recursive: true });
  const root = await fs.mkdtemp(path.join(scratch, "run-"));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const store = await new JobStore(root).init();
  const source = captureNativeSnapshot((options.sourcePaths ?? ["focus.txt"]).map((path) => ({ path, data: Buffer.from("frozen source").toString("base64"), sha256: sha256("frozen source") })));
  const context = capture(history, options, source.record.manifest);
  const job = await store.create({ profile: "harness", provider: "offline", input: { harness: { schemaVersion: 1, mode: options.mode ?? "ask",
    projectId: "project", conversationId: "conversation", source: source.descriptor, history: context.descriptor } } });
  await stageNativeSnapshot(store, job, source);
  await stageNativeHistory(store, job, context);
  return { root, store, job, context, control: await store.files(job.id), service: new HarnessService({ store }), ctx: { jobId: job.id } };
}

test("immutable context retains complete Unicode notes and references privately in all modes", async (t) => {
  for (const mode of ["ask", "plan", "execute"]) {
    const instructions = "喵".repeat(8000), contextNote = "😀".repeat(6000);
    const f = await fixture(t, envelope([]), { instructions, contextNote, contextPaths: ["focus.txt"], mode });
    const result = await f.service.readContext(f.ctx, {});
    assert.equal(result.projectInstructions.content, instructions);
    assert.equal(result.contextNote.content, contextNote);
    assert.equal(result.projectInstructions.contentTruncated, false);
    assert.deepEqual(result.contextPaths, ["focus.txt"]);
    assert.equal(result.scope.available, true);
    assert.ok(encodedBytes(result) < 64_000);
    assert.equal((await readHarnessRecord(f.control, HISTORY_RECORD)).instructions, instructions);
    await assert.rejects((await f.store.workspace(f.job.id)).read(HISTORY_RECORD), { code: "ENOENT" });
  }
});

test("history paging and literal search recall decisions older than the initial recent window", async (t) => {
  const messages = Array.from({ length: 300 }, (_, index) => message(index, index === 14 ? "Decision: use quartz cache; retain the old lifecycle." : `Discussion ${index}`));
  const f = await fixture(t, envelope(messages, 700));
  const search = await f.service.searchHistory(f.ctx, { query: "quartz cache", limit: 10 });
  assert.equal(search.matches.length, 1);
  assert.equal(search.matches[0].index, 714);
  assert.equal(search.matches[0].id, "message-14");
  assert.equal(search.scope.totalMessages, 1000);
  assert.equal(search.scope.upstreamOmittedMessages, 700);
  const page = await f.service.readHistory(f.ctx, { offset: 14, limit: 2 });
  assert.equal(page.messages[0].content, messages[14].content);
  assert.equal(page.messages[1].jobId, "job-15");
  assert.equal(page.nextOffset, 16);
  assert.equal(page.messages[0].index, 714);
  const end = await f.service.readHistory(f.ctx, { offset: 300 });
  assert.deepEqual(end.messages, []);
  assert.equal(end.nextOffset, null);
});

test("large escaped history results remain bounded and expose content truncation", async (t) => {
  const f = await fixture(t, envelope(Array.from({ length: 12 }, (_, index) => message(index, "\u0001".repeat(10_000) + "needle"))));
  const page = await f.service.readHistory(f.ctx, { limit: 50 });
  assert.ok(page.messages.length > 0 && page.messages.length < 12);
  assert.ok(page.messages.every((item) => item.contentTruncated && item.contentCharacters === 10_006));
  assert.ok(encodedBytes(page) < 64_000);
  const found = await f.service.searchHistory(f.ctx, { query: "needle", limit: 100 });
  assert.equal(found.matches.length, 12);
  assert.ok(encodedBytes(found) < 64_000);
  assert.equal(found.matches[0].characterOffset, 10_000);
});

test("single-message content cursors retrieve the complete long captured message with stable identity and scope", async (t) => {
  const text = "Earlier decision:\n" + "😀 quoted context ".repeat(3000) + "\nFINAL_DECISION: retain the lifecycle.";
  const f = await fixture(t, envelope([message(1, text)], 300));
  const parts = [];
  let cursor = 0;
  do {
    const result = await f.service.readHistory(f.ctx, { offset: 0, limit: 1, contentOffset: cursor });
    const item = result.messages[0];
    assert.equal(item.id, "message-1");
    assert.equal(item.index, 300);
    assert.equal(item.jobId, "job-1");
    assert.equal(result.scope.upstreamOmittedMessages, 300);
    assert.equal(item.contentOffset, cursor);
    assert.equal(item.content.isWellFormed(), true);
    assert.ok(encodedBytes(result) < 64_000);
    parts.push(item.content);
    assert.ok(item.nextContentOffset === null || item.nextContentOffset > cursor);
    cursor = item.nextContentOffset;
  } while (cursor !== null);
  assert.ok(parts.length > 1);
  assert.equal(parts.join(""), text);
  assert.equal(parts[0].includes("FINAL_DECISION"), false);
  const end = (await f.service.readHistory(f.ctx, { limit: 1, contentOffset: 256_000 })).messages[0];
  assert.equal(end.content, "");
  assert.equal(end.contentOffset, text.length);
  assert.equal(end.nextContentOffset, null);
});

test("content cursors round surrogate boundaries safely, retain compact receipts and reject multi-message offsets", async (t) => {
  const text = "😀".repeat(12_000) + "tail";
  const f = await fixture(t, envelope([message(0, text), message(1)]));
  const tool = registerHarnessTools(new ToolRegistry(), f.service).get("harness.read_history", ["harness.read_history"]);
  const first = await f.service.readHistory(f.ctx, { limit: 1, contentOffset: 1 });
  const item = first.messages[0];
  assert.equal(item.contentOffset, 0);
  assert.equal(item.content.isWellFormed(), true);
  assert.ok(item.nextContentOffset > 1);
  const next = await f.service.readHistory(f.ctx, { limit: 1, contentOffset: item.nextContentOffset + 1 });
  assert.equal(next.messages[0].contentOffset, item.nextContentOffset);
  assert.equal(next.messages[0].content.isWellFormed(), true);
  assert.ok(next.messages[0].nextContentOffset > item.nextContentOffset + 1);
  const receipt = tool.projectReceipt(first);
  assert.equal(receipt.contentOffset, 0);
  assert.equal(receipt.nextContentOffset, item.nextContentOffset);
  assert.equal(receipt.contentStored, false);
  assert.equal(Object.hasOwn(receipt, "content"), false);
  assert.equal(receipt.controlDigest, f.context.descriptor.controlDigest);
  assert.equal(tool.inputSchema.properties.contentOffset.maximum, 256_000);
  assert.equal(tool.inputSchema.allOf[0].then.properties.limit.const, 1);
  for (const args of [{ contentOffset: 1 }, { limit: 2, contentOffset: 0 }, { limit: 1, contentOffset: -1 },
    { limit: 1, contentOffset: 256_001 }, { limit: 1, contentOffset: 1.5 }]) {
    assert.throws(() => tool.validate(args), { code: "INVALID_ARGUMENTS" });
    await assert.rejects(f.service.readHistory(f.ctx, args), { code: "INVALID_ARGUMENTS" });
  }
});

test("context hash verification rejects altered notes, history and foreign bindings without adopting them", async (t) => {
  for (const alter of [
    (record) => { record.contextNote = "changed"; },
    (record) => { record.conversationHistory.messages[0].content = "changed"; },
    (record) => { record.conversationId = "foreign"; },
    (record) => { record.contextPaths = []; },
  ]) {
    const f = await fixture(t, envelope([message(0)]), { contextNote: "original", contextPaths: ["focus.txt"] });
    const record = await readHarnessRecord(f.control, HISTORY_RECORD);
    alter(record);
    await fs.writeFile(await f.control.resolve(HISTORY_RECORD), JSON.stringify(record));
    await assert.rejects(f.service.readHistory(f.ctx, {}), { code: "HISTORY_SNAPSHOT_CHANGED" });
    await assert.rejects(f.service.readContext(f.ctx, {}), { code: "HISTORY_SNAPSHOT_CHANGED" });
    assert.deepEqual(await readHarnessRecord(f.control, HISTORY_RECORD), record);
  }
});

test("malformed history, duplicate IDs, stale projections and unavailable references fail admission validation", () => {
  const original = envelope([message(0)]);
  for (const value of [
    { ...original, schemaVersion: 2 }, { ...original, totalMessages: 2 }, { ...original, truncated: true },
    envelope([message(0), message(0)]), envelope([{ ...message(0), role: "system" }]),
    envelope([{ ...message(0), id: "x".repeat(600) }]), envelope([{ ...message(0), jobId: {} }]),
    envelope([{ ...message(0), content: "\ud800" }]), envelope(Array.from({ length: 1025 }, (_, index) => message(index))),
    envelope(Array.from({ length: 9 }, (_, index) => message(index, "x".repeat(127_000)))),
  ]) assert.throws(() => capture(value), { code: "INVALID_HARNESS_HISTORY" });
  assert.throws(() => captureNativeHistory({ project: { id: "project" }, conversation: { id: "conversation", messages: [{ role: "user", content: "stale" }] }, conversationHistory: original }), { code: "INVALID_HARNESS_HISTORY" });
  assert.throws(() => capture(original, { contextPaths: ["missing.txt"] }), { code: "CONTEXT_FILE_UNAVAILABLE" });
  for (const value of ["x".repeat(8001), "😀".repeat(8000), "control\u0001", "\ud800"])
    assert.throws(() => capture(original, { instructions: value }), { code: "INVALID_HARNESS_CONTEXT" });
});

test("old jobs and old native captures explicitly expose unavailable history without invented IDs", async (t) => {
  const f = await fixture(t, undefined, { contextNote: "Saved note still retrievable" });
  assert.equal((await f.service.readContext(f.ctx, {})).contextNote.content, "Saved note still retrievable");
  assert.equal((await f.service.readHistory(f.ctx, {})).scope.available, false);
  const job = await f.store.load(f.job.id); delete job.input.harness.history; await f.store.save(job);
  const result = await f.service.readContext(f.ctx, {});
  assert.equal(result.contextAvailable, false);
  assert.equal(result.projectInstructions, null);
  assert.equal(result.scope.reason, "history_not_captured");
  assert.deepEqual((await f.service.searchHistory(f.ctx, { query: "absent" })).matches, []);
});

test("a native transport budget may capture an empty suffix with every older message explicitly omitted", async (t) => {
  const history = envelope([], 600);
  const captured = captureNativeHistory({ project: { id: "project" }, conversation: { id: "conversation",
    messages: Array.from({ length: 8 }, (_, index) => ({ role: "user", content: `compact ${index}` })), omittedMessages: 592 }, conversationHistory: history });
  assert.equal(captured.descriptor.messageCount, 0);
  assert.equal(captured.descriptor.startIndex, 600);
  const f = await fixture(t, history);
  const result = await f.service.readHistory(f.ctx, {});
  assert.deepEqual(result.messages, []);
  assert.equal(result.nextOffset, null);
  assert.equal(result.scope.available, true);
  assert.equal(result.scope.totalMessages, 600);
  assert.equal(result.scope.upstreamOmittedMessages, 600);
  assert.equal(result.scope.upstreamTruncated, true);
  assert.deepEqual((await f.service.searchHistory(f.ctx, { query: "compact" })).matches, []);
});


test("maximum notes and JSON-escaped focused paths paginate within tool output bounds", async (t) => {
  const paths = Array.from({ length: 20 }, (_, index) => `focus/${index}-` + ['"'.repeat(160), '"'.repeat(160), '"'.repeat(160)].join("/"));
  const instructions = "文".repeat(8000), contextNote = "字".repeat(8000);
  const f = await fixture(t, envelope([]), { sourcePaths: paths, contextPaths: paths, instructions, contextNote });
  const first = await f.service.readContext(f.ctx, {});
  assert.ok(first.nextPathOffset > 0 && first.nextPathOffset < paths.length);
  assert.equal(first.contextPathsTruncated, true);
  assert.ok(encodedBytes(first) < 64_000);
  const next = await f.service.readContext(f.ctx, { pathOffset: first.nextPathOffset });
  assert.equal(next.nextPathOffset, null);
  assert.equal(first.projectInstructions.content.length, instructions.length);
  assert.equal(next.contextNote.content.length, contextNote.length);
  assert.deepEqual([...first.contextPaths, ...next.contextPaths], paths);
  assert.ok(encodedBytes(next) < 64_000);
});
