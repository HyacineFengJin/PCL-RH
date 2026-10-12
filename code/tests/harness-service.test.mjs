import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { JobStore } from "../../experimental/runtime/src/store.mjs";
import { ToolRegistry } from "../../experimental/runtime/src/tools.mjs";
import { registerHarnessTools } from "../../experimental/runtime/src/harness-tools.mjs";
import { HarnessService } from "../harness-service.mjs";
import { captureNativeSnapshot, stageNativeSnapshot, sha256, recordDigest, readHarnessRecord,
  writeHarnessRecord, writeHarnessBytes, SNAPSHOT_RECORD } from "../harness-snapshot.mjs";

const scratch = fileURLToPath(new URL("../../work/harness-2026-10-12/service-tests/", import.meta.url));
const entry = (path, text) => ({ path, data: Buffer.from(text).toString("base64"), sha256: sha256(text) });
async function fixture(t, { files = { "src/A.txt": "original\n" }, protectedPaths = [], mode = "execute", status = "completed", plan = true } = {}) {
  await fs.mkdir(scratch, { recursive: true });
  const root = await fs.mkdtemp(path.join(scratch, "run-"));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const store = await new JobStore(root).init(), snapshot = captureNativeSnapshot(Object.entries(files).map(([name, text]) => entry(name, text)), protectedPaths);
  const job = await store.create({ profile: "harness", provider: "offline", status,
    input: { harness: { schemaVersion: 1, mode, pluginId: "rh.project", projectId: "project", conversationId: "conversation", source: snapshot.descriptor } },
    operations: plan ? [{ id: "plan", name: "harness.report_plan", status: "completed", result: { steps: ["Inspect", "Propose"] } }] : [] });
  await stageNativeSnapshot(store, job, snapshot);
  const service = new HarnessService({ store }), workspace = await store.workspace(job.id), control = await store.files(job.id);
  return { root, store, job, service, workspace, control, snapshot,
    ctx: { jobId: job.id, operationId: "proposal", signal: new AbortController().signal } };
}
const change = (text = "updated\n") => ({ summary: "Update A", changes: [{ path: "src/A.txt", expected_sha256: sha256("original\n"), content: text }] });
async function propose(f, args = change(), operationId = "proposal") {
  const result = await f.service.proposeChanges({ ...f.ctx, operationId }, args);
  const job = await f.store.load(f.job.id);
  if (!job.operations.some((operation) => operation.id === operationId)) {
    job.operations.push({ id: operationId, name: "harness.propose_changes", status: "completed", result });
    await f.store.save(job);
  }
  return result;
}

test("manifest fingerprint uses exact path/sha256/size keys and UTF-8 byte path ordering", () => {
  const input = [entry("😀.txt", "emoji"), entry("ä.txt", "accent"), entry("z.txt", "plain")];
  const snapshot = captureNativeSnapshot(input);
  assert.deepEqual(snapshot.record.manifest.map((file) => file.path), ["z.txt", "ä.txt", "😀.txt"]);
  assert.ok(snapshot.record.manifest.every((file) => Object.keys(file).join() === "path,sha256,size"));
  assert.equal(snapshot.descriptor.fingerprint, sha256(JSON.stringify(snapshot.record.manifest)));
  assert.equal(snapshot.descriptor.fingerprint, captureNativeSnapshot(input.reverse()).descriptor.fingerprint);
});
test("snapshot rejects hidden paths, malformed UTF-8, duplicate files and ambiguous file trees", () => {
  for (const name of [".env", ".git/config", "src/.secret/file", "../outside", "/absolute", "C:/file", "src\\file", "a//b", "a/./b", "\ud800.txt", "\u0085.txt", "文".repeat(200)])
    assert.throws(() => captureNativeSnapshot([entry(name, "text")]), { code: "INVALID_SOURCE_PATH" });
  const invalid = Buffer.from([0xff]);
  assert.throws(() => captureNativeSnapshot([{ path: "text.txt", data: invalid.toString("base64"), sha256: sha256(invalid) }]), { code: "INVALID_SOURCE_TEXT" });
  assert.throws(() => captureNativeSnapshot([entry("text.txt", "valid\0binary")]), { code: "INVALID_SOURCE_TEXT" });
  assert.throws(() => captureNativeSnapshot([entry("a.txt", "a"), entry("a.txt", "b")]), { code: "INVALID_SOURCE_SNAPSHOT" });
  assert.throws(() => captureNativeSnapshot([entry("a", "a"), entry("a/b", "b")]), { code: "INVALID_SOURCE_SNAPSHOT" });
});
test("native dependency/generated exclusions and NUL text are refused before model review", async (t) => {
  const f = await fixture(t), registry = registerHarnessTools(new ToolRegistry(), f.service);
  const read = registry.get("harness.read_file", ["harness.read_file"]);
  const preview = registry.get("harness.propose_changes", ["harness.propose_changes"]);
  for (const segment of ["node_modules", "target", "build", "dist", "vendor", "venv", "__pycache__"]) {
    const path = `src/${segment}/file.txt`;
    assert.throws(() => captureNativeSnapshot([entry(path, "dependency")]), { code: "INVALID_SOURCE_PATH" });
    assert.throws(() => read.validate({ path }), { code: "UNSAFE_PATH" });
    await assert.rejects(f.service.readFile(f.ctx, { path }), { code: "INVALID_SOURCE_PATH" });
    const args = { summary: "Excluded source", changes: [{ path, expected_sha256: null, content: "content" }] };
    assert.throws(() => preview.validate(args), { code: "UNSAFE_PATH" });
    await assert.rejects(f.service.proposeChanges(f.ctx, args), { code: "INVALID_SOURCE_PATH" });
  }
  assert.equal(captureNativeSnapshot([entry("build.gradle", "visible build configuration"), entry("Build/source.txt", "case-sensitive name")]).descriptor.fileCount, 2);
  assert.throws(() => preview.validate(change("text\0binary")), { code: "INVALID_ARGUMENTS" });
  await assert.rejects(f.service.proposeChanges(f.ctx, change("text\0binary")), { code: "INVALID_PROPOSAL" });
  await assert.rejects(fs.stat(path.join(f.root, f.job.id, "proposal-files")), { code: "ENOENT" });
});
test("inspection is bound to the original manifest and paginates even escaped text within receipt limits", async (t) => {
  const f = await fixture(t, { files: { "a.txt": "a", "b.txt": "b", "src/A.txt": "\u0001".repeat(16000) + "needle\nneedle" } });
  const first = await f.service.listFiles(f.ctx, { offset: 0, limit: 1 });
  assert.equal(first.files.length, 1); assert.equal(first.nextOffset, 1); assert.equal(first.total, 3);
  const next = await f.service.listFiles(f.ctx, { offset: first.nextOffset, limit: 100 });
  assert.equal(next.files.length, 2); assert.equal(next.nextOffset, null);
  const read = await f.service.readFile(f.ctx, { path: "src/A.txt", limit: 16000 });
  assert.ok(read.content.length > 0 && read.content.length < 16000);
  assert.ok(Buffer.byteLength(JSON.stringify(read)) < 64_000);
  assert.equal(read.nextOffset, read.content.length); assert.equal(read.truncated, true);
  const search = await f.service.searchFiles(f.ctx, { query: "needle", limit: 1 });
  assert.equal(search.matches.length, 1); assert.equal(search.truncated, true);
  await assert.rejects(f.service.readFile(f.ctx, { path: "outside.txt" }), { code: "SOURCE_FILE_UNAVAILABLE" });
});
test("source edits, symlink replacements and multiple links refuse immutable snapshot reads", async (t) => {
  const edited = await fixture(t);
  await fs.writeFile(await edited.workspace.resolve("source/src/A.txt"), "changed\n");
  await assert.rejects(edited.service.readFile(edited.ctx, { path: "src/A.txt" }), { code: "SOURCE_SNAPSHOT_CHANGED" });
  const linked = await fixture(t), target = await linked.workspace.resolve("source/src/A.txt");
  await fs.writeFile(path.join(linked.root, "external.txt"), "original\n");
  await fs.rm(target); await fs.symlink(path.join(linked.root, "external.txt"), target);
  await assert.rejects(linked.service.readFile(linked.ctx, { path: "src/A.txt" }), { code: "UNSAFE_PATH" });
  const hardlinked = await fixture(t);
  await fs.link(await hardlinked.workspace.resolve("source/src/A.txt"), path.join(hardlinked.root, "other-link.txt"));
  await assert.rejects(hardlinked.service.readFile(hardlinked.ctx, { path: "src/A.txt" }), { code: "UNSAFE_SOURCE" });
});
test("snapshot control digest conflicts are retained and not silently adopted", async (t) => {
  const f = await fixture(t), record = await readHarnessRecord(f.control, SNAPSHOT_RECORD);
  record.manifest[0].sha256 = "0".repeat(64);
  await f.control.write(SNAPSHOT_RECORD, JSON.stringify(record));
  await assert.rejects(f.service.listFiles(f.ctx, {}), { code: "SOURCE_SNAPSHOT_CHANGED" });
  assert.equal((await readHarnessRecord(f.control, SNAPSHOT_RECORD)).manifest[0].sha256, "0".repeat(64));
});
test("Ask and Plan cannot propose; Execute requires a durable plan before any private proposal files", async (t) => {
  const ask = await fixture(t, { mode: "ask" }), plan = await fixture(t, { mode: "plan" }), execute = await fixture(t, { plan: false });
  await assert.rejects(ask.service.reportPlan(ask.ctx, { summary: "Plan", steps: ["Step"] }), { code: "TOOL_DENIED" });
  await assert.rejects(ask.service.proposeChanges(ask.ctx, change()), { code: "TOOL_DENIED" });
  assert.equal((await plan.service.reportPlan(plan.ctx, { summary: "Plan", steps: ["Step"] })).status, "plan_recorded");
  await assert.rejects(plan.service.proposeChanges(plan.ctx, change()), { code: "TOOL_DENIED" });
  await assert.rejects(execute.service.proposeChanges(execute.ctx, change()), { code: "PLAN_REQUIRED" });
  await assert.rejects(fs.stat(path.join(execute.root, execute.job.id, "proposal-files")), { code: "ENOENT" });
  const names = registerHarnessTools(new ToolRegistry(), execute.service).describe(["harness.propose_changes", "harness.mark_applied", "harness.export", "harness.list_reviews"]);
  assert.deepEqual(names.map((tool) => tool.name), ["harness.propose_changes"]);
});
test("handwritten and locked Maker receipts protect source without rewriting the original ownership metadata", async (t) => {
  const receipt = JSON.stringify({ schema_version: 1, files: {
    "src/A.txt": { owner: "template", sha256: sha256("original\n") },
    "src/Handwritten.txt": { owner: "template", sha256: sha256("older\n") },
    "src/Locked.txt": { owner: "locked", sha256: sha256("locked\n") },
  } });
  const f = await fixture(t, { files: { "src/A.txt": "original\n", "src/Handwritten.txt": "handwritten\n", "src/Locked.txt": "locked\n", "creator-ownership.json": receipt } });
  assert.deepEqual(f.snapshot.record.protectedPaths, ["creator-ownership.json", "src/Handwritten.txt", "src/Locked.txt"]);
  for (const [name, text] of [["src/Handwritten.txt", "handwritten\n"], ["src/Locked.txt", "locked\n"], ["creator-ownership.json", receipt]])
    await assert.rejects(f.service.proposeChanges(f.ctx, { summary: "Cannot replace handwritten source", changes: [{ path: name, expected_sha256: sha256(text), content: "replace" }] }), { code: "HANDWRITTEN_PROTECTED" });
  assert.equal((await f.workspace.read("source/creator-ownership.json")).content, receipt);
  const malformed = await fixture(t, { files: { "src/A.txt": "original\n", "creator-ownership.json": "broken receipt" } });
  await assert.rejects(malformed.service.proposeChanges(malformed.ctx, change()), { code: "HANDWRITTEN_PROTECTED" });
  assert.equal((await malformed.service.readFile(malformed.ctx, { path: "src/A.txt" })).content, "original\n");
});
test("proposal hashes are exact and an interrupted identical staging operation can retry without overwrites", async (t) => {
  const f = await fixture(t);
  await assert.rejects(f.service.proposeChanges(f.ctx, { ...change(), changes: [{ ...change().changes[0], expected_sha256: "0".repeat(64) }] }), { code: "SOURCE_HASH_MISMATCH" });
  await assert.rejects(f.service.proposeChanges(f.ctx, change("\ud800")), { code: "INVALID_PROPOSAL" });
  const id = sha256(`${f.job.id}:proposal`).slice(0, 32);
  await writeHarnessBytes(f.control, `proposal-files/${id}/src/A.txt`, Buffer.from("updated\n"));
  const result = await propose(f);
  assert.equal(result.proposalId, id); assert.equal(result.status, "pending");
  assert.deepEqual(await f.service.proposeChanges(f.ctx, change()), result);
  await assert.rejects(f.service.proposeChanges(f.ctx, change("different\n")), { code: "OPERATION_MISMATCH" });
  assert.equal((await f.workspace.read("source/src/A.txt")).content, "original\n");
  const conflicting = await fixture(t), conflictId = sha256(`${conflicting.job.id}:proposal`).slice(0, 32);
  await writeHarnessBytes(conflicting.control, `proposal-files/${conflictId}/src/A.txt`, Buffer.from("other staged bytes"));
  await assert.rejects(conflicting.service.proposeChanges(conflicting.ctx, change()), { code: "SOURCE_SNAPSHOT_CHANGED" });
});
test("review projections provide creation/modification/deletion fields and explicit bounded text flags", async (t) => {
  const f = await fixture(t, { files: { "src/A.txt": "original\n", "delete.txt": "delete me\n" } });
  const result = await propose(f, { summary: "Three reviewed changes", changes: [change().changes[0],
    { path: "new.txt", expected_sha256: null, content: "x".repeat(20000) }, { path: "delete.txt", expected_sha256: sha256("delete me\n"), content: null }] });
  const reviews = await f.service.listReviews(f.job.id), review = reviews.proposals[0];
  assert.equal(review.id, result.proposalId); assert.equal(review.digest, result.digest); assert.equal(review.status, "pending");
  assert.deepEqual(review.changes.map((file) => file.kind), ["delete", "create", "modify"]);
  const created = review.changes.find((file) => file.path === "new.txt");
  assert.equal(created.before, null); assert.equal(created.beforeSha256, null); assert.equal(created.afterTruncated, true);
  assert.equal(created.after.length, 16000); assert.equal(reviews.truncated, true);
  assert.ok(Buffer.byteLength(JSON.stringify(reviews)) < 512_000);
  const exported = await f.service.exportProposal(f.job.id, { proposalId: result.proposalId, expectedDigest: result.digest });
  assert.equal(Buffer.from(exported.files.find((file) => file.path === "new.txt").data, "base64").length, 20000);
});
test("export rechecks all frozen source, proposal bytes and the reviewed digest", async (t) => {
  const f = await fixture(t, { files: { "src/A.txt": "original\n", "untouched.txt": "must remain unchanged\n" } }), result = await propose(f);
  await assert.rejects(f.service.exportProposal(f.job.id, { proposalId: result.proposalId, expectedDigest: "0".repeat(64) }), { code: "STALE_APPROVAL" });
  await fs.writeFile(await f.workspace.resolve("source/untouched.txt"), "changed\n");
  await assert.rejects(f.service.exportProposal(f.job.id, { proposalId: result.proposalId, expectedDigest: result.digest }), { code: "SOURCE_SNAPSHOT_CHANGED" });
  const changed = await fixture(t), changedResult = await propose(changed);
  await fs.writeFile(await changed.control.resolve(`proposal-files/${changedResult.proposalId}/src/A.txt`), "tampered\n");
  await assert.rejects(changed.service.exportProposal(changed.job.id, { proposalId: changedResult.proposalId, expectedDigest: changedResult.digest }), { code: "SOURCE_SNAPSHOT_CHANGED" });
  const metadata = await fixture(t), metadataResult = await propose(metadata), recordPath = `control/harness-proposals/${metadataResult.proposalId}.json`;
  const record = await readHarnessRecord(metadata.control, recordPath); record.summary = "mutated";
  await metadata.control.write(recordPath, JSON.stringify(record));
  await assert.rejects(metadata.service.getReview(metadata.job.id, metadataResult.proposalId), { code: "PROPOSAL_CHANGED" });
});
test("host review/export rejects a digest-consistent legacy proposal containing NUL", async (t) => {
  const f = await fixture(t), result = await propose(f), recordPath = `control/harness-proposals/${result.proposalId}.json`;
  const bytes = Buffer.from("legacy\0proposal"), record = await readHarnessRecord(f.control, recordPath);
  record.changes[0].sha256 = sha256(bytes); record.changes[0].size = bytes.length;
  record.digest = recordDigest({ schemaVersion: record.schemaVersion, proposalId: record.proposalId, jobId: record.jobId,
    projectId: record.projectId, conversationId: record.conversationId, sourceFingerprint: record.sourceFingerprint,
    summary: record.summary, changes: record.changes });
  await fs.writeFile(await f.control.resolve(`proposal-files/${result.proposalId}/src/A.txt`), bytes);
  await f.control.write(recordPath, JSON.stringify(record));
  await assert.rejects(f.service.getReview(f.job.id, result.proposalId), { code: "INVALID_SOURCE_TEXT" });
  await assert.rejects(f.service.exportProposal(f.job.id, { proposalId: result.proposalId, expectedDigest: record.digest }), { code: "INVALID_SOURCE_TEXT" });
});
test("failed, cancelled and unfinished jobs cannot export or mark source application", async (t) => {
  const f = await fixture(t), result = await propose(f);
  for (const status of ["queued", "running", "failed", "cancelled", "interrupted", "unknown"]) {
    const job = await f.store.load(f.job.id); job.status = status; await f.store.save(job);
    const args = { proposalId: result.proposalId, expectedDigest: result.digest };
    await assert.rejects(f.service.exportProposal(job.id, args), { code: "PROPOSAL_NOT_READY" });
    await assert.rejects(f.service.markApplied(job.id, args), { code: "PROPOSAL_NOT_READY" });
    assert.equal((await f.service.listReviews(job.id)).proposals[0].status, "pending");
  }
});
test("native applied receipts are digest-bound, idempotent and do not block historical review reads", async (t) => {
  const f = await fixture(t), result = await propose(f), args = { proposalId: result.proposalId, expectedDigest: result.digest };
  await assert.rejects(f.service.markApplied(f.job.id, { ...args, expectedDigest: "0".repeat(64) }), { code: "STALE_APPROVAL" });
  const before = (await f.store.load(f.job.id)).revision, applied = await f.service.markApplied(f.job.id, args);
  assert.equal(applied.status, "applied");
  const after = (await f.store.load(f.job.id)).revision; assert.ok(after > before);
  assert.deepEqual(await f.service.markApplied(f.job.id, args), applied);
  assert.equal((await f.store.load(f.job.id)).revision, after);
  assert.equal((await f.service.getReview(f.job.id, result.proposalId)).status, "applied");
  assert.equal((await f.service.listReviews(f.job.id)).proposals[0].status, "applied");
  assert.equal((await f.service.exportProposal(f.job.id, args)).digest, result.digest);
  assert.equal((await f.workspace.read("source/src/A.txt")).content, "original\n");
});
test("concurrent host acknowledgement retries adopt one digest-bound applied receipt", async (t) => {
  const f = await fixture(t), result = await propose(f), args = { proposalId: result.proposalId, expectedDigest: result.digest };
  const [first, second] = await Promise.all([f.service.markApplied(f.job.id, args), new HarnessService({ store: f.store }).markApplied(f.job.id, args)]);
  assert.deepEqual(first, second);
  assert.equal((await f.service.getReview(f.job.id, result.proposalId)).appliedAt, first.appliedAt);
});
test("the largest long-path protected manifest fits private control storage without increasing source-file limits", async (t) => {
  const f = await fixture(t, { files: {} });
  const prefix = Array.from({ length: 4 }, (_, index) => `${index}${"a".repeat(119)}`).join("/");
  const files = Array.from({ length: 1000 }, (_, index) => entry(`${prefix}/${index}.txt`, ""));
  const snapshot = captureNativeSnapshot(files, files.map((file) => file.path));
  assert.ok(Buffer.byteLength(JSON.stringify(snapshot.record)) > 1_000_000);
  const job = await f.store.load(f.job.id); job.input.harness.source = snapshot.descriptor; await f.store.save(job);
  // Metadata validation does not pretend to have read source files. Actual file
  // tools and export still require every staged byte and its original hash.
  await f.control.write(SNAPSHOT_RECORD, "{}");
  await fs.rm(await f.control.resolve(SNAPSHOT_RECORD));
  await writeHarnessRecord(f.control, SNAPSHOT_RECORD, snapshot.record);
  assert.equal((await f.service.snapshot(job.id)).entries.size, 1000);
  assert.equal(recordDigest(await readHarnessRecord(f.control, SNAPSHOT_RECORD)), snapshot.descriptor.controlDigest);
});
