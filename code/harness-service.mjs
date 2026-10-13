/** Host service for generic RH Code jobs. Model tools can inspect a frozen
 * source and propose changes; review/export remain host-only capabilities.
 * Proposed bytes live outside the model workspace. Export rechecks both the
 * frozen inputs and proposal hashes so Rust never adopts model-supplied bytes. */
import { check, throwIfAborted } from "../experimental/runtime/src/errors.mjs";
import {
  HARNESS_LIMITS, SNAPSHOT_RECORD, recordDigest, sha256, exactHash, compareSourcePaths,
  sourcePath, decodeText, readSnapshotBytes, writeHarnessBytes,
  readHarnessRecord, writeHarnessRecord,
} from "./harness-snapshot.mjs";
import { readNativeHistory, boundedExcerpt, HISTORY_LIMITS } from "./history-snapshot.mjs";

const PROPOSAL_ID = /^[a-f0-9]{32}$/;
const MODES = ["ask", "plan", "execute"];
const outputBytes = (value) => Buffer.byteLength(JSON.stringify(value));
const reviewPath = (id) => {
  check(typeof id === "string" && PROPOSAL_ID.test(id), "INVALID_PROPOSAL", "Invalid proposal identity");
  return `control/harness-proposals/${id}.json`;
};
const proposalDigest = (record) => recordDigest({
  schemaVersion: record.schemaVersion, proposalId: record.proposalId, jobId: record.jobId,
  projectId: record.projectId, conversationId: record.conversationId,
  sourceFingerprint: record.sourceFingerprint, summary: record.summary, changes: record.changes,
});
const appliedPath = (id) => { reviewPath(id); return `control/harness-applied/${id}.json`; };
function textPreview(text, maximumBytes) {
  let low = 0, high = Math.min(text.length, 16_000);
  while (low < high) {
    const length = Math.ceil((low + high) / 2);
    if (outputBytes(text.slice(0, length)) <= maximumBytes) low = length;
    else high = length - 1;
  }
  return { text: text.slice(0, low), truncated: low < text.length };
}
function historyScope(record) {
  const history = record?.conversationHistory;
  return history ? { available: true, capturedMessages: history.messages.length, startIndex: history.startIndex,
    totalMessages: history.totalMessages, upstreamOmittedMessages: history.startIndex, upstreamTruncated: history.truncated }
    : { available: false, capturedMessages: 0, startIndex: null, totalMessages: null,
      upstreamOmittedMessages: null, upstreamTruncated: null, reason: "history_not_captured" };
}

export class HarnessService {
  constructor({ store }) {
    check(store && typeof store.load === "function" && typeof store.files === "function",
      "INVALID_HARNESS_HOST", "Harness requires the private job store");
    this.store = store;
  }

  async snapshot(jobId) {
    const job = await this.store.load(jobId);
    const harness = job.input?.harness;
    check(job.profile === "harness" && harness?.schemaVersion === 1 && MODES.includes(harness.mode) &&
      typeof harness.projectId === "string" && typeof harness.conversationId === "string" &&
      harness.source?.directory === "source" && exactHash(harness.source.fingerprint) && exactHash(harness.source.controlDigest),
    "INVALID_HARNESS_JOB", "Job has no native-bound Harness snapshot");
    const control = await this.store.files(jobId);
    const record = await readHarnessRecord(control, SNAPSHOT_RECORD);
    check(record.schemaVersion === 1 && Array.isArray(record.manifest) && Array.isArray(record.protectedPaths) &&
      record.manifest.length <= HARNESS_LIMITS.files && recordDigest(record) === harness.source.controlDigest &&
      recordDigest(record.manifest) === harness.source.fingerprint,
    "SOURCE_SNAPSHOT_CHANGED", "The immutable source manifest changed");
    const entries = new Map();
    let totalBytes = 0;
    let previousPath;
    for (const entry of record.manifest) {
      sourcePath(entry.path);
      check(Object.keys(entry).sort().join() === "path,sha256,size" &&
        (previousPath === undefined || compareSourcePaths(previousPath, entry.path) < 0) &&
        !entries.has(entry.path) && exactHash(entry.sha256) && Number.isSafeInteger(entry.size) &&
        entry.size >= 0 && entry.size <= HARNESS_LIMITS.fileBytes,
      "SOURCE_SNAPSHOT_CHANGED", "Invalid frozen source entry");
      entries.set(entry.path, entry);
      previousPath = entry.path;
      totalBytes += entry.size;
    }
    check(totalBytes <= HARNESS_LIMITS.sourceBytes && entries.size === harness.source.fileCount &&
      totalBytes === harness.source.totalBytes && record.protectedPaths.every((name) => entries.has(name)),
    "SOURCE_SNAPSHOT_CHANGED", "Frozen source inventory differs from admission");
    return { job, harness, record, entries, control, workspace: await this.store.workspace(jobId) };
  }

  async #context(ctx) {
    throwIfAborted(ctx.signal);
    return this.snapshot(ctx.jobId);
  }
  #prefix(prefix = "") {
    check(typeof prefix === "string" && prefix.length <= 512, "INVALID_ARGUMENTS", "Invalid path prefix");
    if (prefix) sourcePath(prefix.endsWith("/") ? prefix.slice(0, -1) : prefix);
    return prefix;
  }

  async listFiles(ctx, { prefix = "", offset = 0, limit = 50 } = {}) {
    prefix = this.#prefix(prefix);
    check(Number.isSafeInteger(offset) && offset >= 0 && Number.isSafeInteger(limit) && limit > 0 && limit <= 100,
      "INVALID_ARGUMENTS", "Invalid file page");
    const { record } = await this.#context(ctx);
    const eligible = record.manifest.filter((entry) => entry.path.startsWith(prefix));
    const files = [];
    for (const entry of eligible.slice(offset, offset + limit)) {
      if (outputBytes([...files, entry]) > 50_000) break;
      files.push(entry);
    }
    const next = offset + files.length;
    return { files, total: eligible.length, nextOffset: next < eligible.length ? next : null };
  }

  async readFile(ctx, { path, offset = 0, limit = 8000 }) {
    sourcePath(path);
    check(Number.isSafeInteger(offset) && offset >= 0 && offset <= HARNESS_LIMITS.fileBytes &&
      Number.isSafeInteger(limit) && limit >= 1 && limit <= 16000,
    "INVALID_ARGUMENTS", "Read uses a nonnegative character offset and at most 16000 characters");
    const snapshot = await this.#context(ctx);
    const entry = snapshot.entries.get(path);
    check(entry, "SOURCE_FILE_UNAVAILABLE", "Path is outside the frozen native source");
    const text = decodeText(await readSnapshotBytes(snapshot.workspace, `source/${path}`, entry));
    const excerpt = textPreview(text.slice(offset, offset + limit), 48_000);
    const content = excerpt.text;
    const next = offset + content.length;
    return { path, sha256: entry.sha256, size: entry.size, offset, content,
      nextOffset: next < text.length ? next : null, truncated: next < text.length,
      protected: snapshot.record.protectedPaths.includes(path) };
  }

  async searchFiles(ctx, { query, pathPrefix = "", limit = 50 }) {
    check(typeof query === "string" && query.length > 0 && query.length <= 200 && !query.includes("\n"),
      "INVALID_ARGUMENTS", "Search requires a literal query of at most 200 characters");
    pathPrefix = this.#prefix(pathPrefix);
    check(Number.isSafeInteger(limit) && limit > 0 && limit <= 100, "INVALID_ARGUMENTS", "Invalid search limit");
    const snapshot = await this.#context(ctx);
    const matches = [];
    let truncated = false;
    for (const entry of snapshot.record.manifest) {
      throwIfAborted(ctx.signal);
      if (!entry.path.startsWith(pathPrefix)) continue;
      const text = decodeText(await readSnapshotBytes(snapshot.workspace, `source/${entry.path}`, entry));
      const lines = text.split("\n");
      for (let i = 0; i < lines.length; i++) {
        const column = lines[i].indexOf(query);
        if (column < 0) continue;
        const match = { path: entry.path, line: i + 1, column: column + 1,
          excerpt: lines[i].slice(Math.max(0, column - 60), column + query.length + 80) };
        if (matches.length >= limit || outputBytes([...matches, match]) > 50_000) { truncated = true; break; }
        matches.push(match);
      }
      if (truncated) break;
    }
    return { query, matches, truncated };
  }

  async #history(ctx) {
    const snapshot = await this.#context(ctx);
    const record = await readNativeHistory(snapshot);
    return { record, scope: historyScope(record), controlDigest: snapshot.harness.history?.controlDigest ?? null };
  }
  async readContext(ctx, { pathOffset = 0 } = {}) {
    check(Number.isSafeInteger(pathOffset) && pathOffset >= 0 && pathOffset <= HISTORY_LIMITS.contextPaths,
      "INVALID_ARGUMENTS", "Context file references use an offset from 0 to 20");
    const { record, scope, controlDigest } = await this.#history(ctx);
    const projected = (value) => {
      if (value === undefined) return null;
      // Native notes allow 24000 UTF-8 bytes. Two JSON quote bytes must not
      // silently drop the final character of a valid maximum-size note.
      const content = boundedExcerpt(value, value.length, 24_002);
      return { content, contentCharacters: value.length, contentTruncated: content.length < value.length };
    };
    const result = { label: "FROZEN_NATIVE_CONTEXT", quotedContext: true, contextAvailable: record !== null, controlDigest,
      projectInstructions: projected(record?.instructions), contextNote: projected(record?.contextNote),
      contextPaths: [], contextPathsTotal: record?.contextPaths.length ?? 0, pathOffset, nextPathOffset: null,
      contextPathsTruncated: false, contextPathsMeaning: "Focused references within the frozen source; no permission change", scope };
    for (const path of (record?.contextPaths ?? []).slice(pathOffset)) {
      if (outputBytes({ ...result, contextPaths: [...result.contextPaths, path] }) > 62_000) break;
      result.contextPaths.push(path);
    }
    const next = pathOffset + result.contextPaths.length;
    result.nextPathOffset = next < result.contextPathsTotal ? next : null;
    result.contextPathsTruncated = pathOffset > 0 || result.nextPathOffset !== null;
    return result;
  }
  async readHistory(ctx, { offset = 0, limit = 20, contentOffset } = {}) {
    check(Number.isSafeInteger(offset) && offset >= 0 && offset <= HISTORY_LIMITS.messages &&
      Number.isSafeInteger(limit) && limit >= 1 && limit <= 50,
    "INVALID_ARGUMENTS", "History uses a captured-message offset and at most 50 messages");
    check(contentOffset === undefined || (limit === 1 && Number.isSafeInteger(contentOffset) && contentOffset >= 0 && contentOffset <= 256_000),
      "INVALID_ARGUMENTS", "History contentOffset uses UTF-16 code units from 0 to 256000 and requires limit=1");
    const { record, scope, controlDigest } = await this.#history(ctx);
    const captured = record?.conversationHistory?.messages ?? [];
    const messages = [];
    for (let cursor = offset; cursor < Math.min(captured.length, offset + limit); cursor++) {
      throwIfAborted(ctx.signal);
      const message = captured[cursor];
      // Cursor units match JavaScript file reads. A manually supplied offset
      // inside a surrogate pair rounds back to include the complete character;
      // returned cursors always advance across complete Unicode characters.
      let start = Math.min(contentOffset ?? 0, message.content.length);
      if (start > 0 && /[\uDC00-\uDFFF]/.test(message.content[start])) start--;
      const content = boundedExcerpt(message.content.slice(start), 16_000, 16_000);
      const contentEnd = start + content.length;
      const projected = { index: scope.startIndex + cursor, id: message.id, role: message.role, content, jobId: message.jobId,
        contentCharacters: message.content.length, contentOffset: start,
        nextContentOffset: contentEnd < message.content.length ? contentEnd : null,
        contentTruncated: start > 0 || contentEnd < message.content.length };
      if (outputBytes([...messages, projected]) > 50_000) break;
      messages.push(projected);
    }
    const next = offset + messages.length;
    return { label: "FROZEN_NATIVE_HISTORY", quotedContext: true, controlDigest, scope, offset, messages,
      nextOffset: next < captured.length ? next : null, truncated: next < captured.length || messages.some((message) => message.contentTruncated) };
  }
  async searchHistory(ctx, { query, limit = 50 }) {
    check(typeof query === "string" && query.isWellFormed() && query.length > 0 && query.length <= 200 &&
      Number.isSafeInteger(limit) && limit >= 1 && limit <= 100,
    "INVALID_ARGUMENTS", "History search requires a literal query and at most 100 matches");
    const { record, scope, controlDigest } = await this.#history(ctx);
    const matches = [];
    let truncated = false;
    outer: for (const [cursor, message] of (record?.conversationHistory?.messages ?? []).entries()) {
      throwIfAborted(ctx.signal);
      let from = 0, characterOffset;
      while ((characterOffset = message.content.indexOf(query, from)) >= 0) {
        let excerptStart = Math.max(0, characterOffset - 80);
        if (excerptStart > 0 && /[\uDC00-\uDFFF]/.test(message.content[excerptStart])) excerptStart--;
        const candidate = { index: scope.startIndex + cursor, id: message.id, role: message.role, jobId: message.jobId,
          characterOffset, excerptStart, excerpt: boundedExcerpt(message.content.slice(excerptStart, characterOffset + query.length + 80), 360, 2400) };
        if (matches.length >= limit || outputBytes([...matches, candidate]) > 50_000) { truncated = true; break outer; }
        matches.push(candidate);
        from = characterOffset + query.length;
      }
    }
    return { label: "FROZEN_NATIVE_HISTORY_SEARCH", quotedContext: true, controlDigest, scope, query, matches, truncated };
  }

  async reportPlan(ctx, { summary, steps }) {
    const { harness } = await this.#context(ctx);
    check(harness.mode !== "ask", "TOOL_DENIED", "Ask mode cannot record an execution plan");
    check(typeof summary === "string" && summary.trim() && summary.length <= 2000 &&
      Array.isArray(steps) && steps.length > 0 && steps.length <= 40 &&
      steps.every((step) => typeof step === "string" && step.trim() && step.length <= 1000) &&
      outputBytes({ summary, steps }) <= 16_000,
    "INVALID_PLAN", "Expected a concise summary and actionable steps");
    return { status: "plan_recorded", summary, steps, buildValidated: false };
  }

  async proposeChanges(ctx, { summary, changes }) {
    const snapshot = await this.#context(ctx);
    const { job, harness, entries, record, control } = snapshot;
    check(harness.mode === "execute", "TOOL_DENIED", "Only Execute can propose source changes");
    check(job.operations.some((op) => op.name === "harness.report_plan" && op.status === "completed"),
      "PLAN_REQUIRED", "Record a plan before proposing source changes");
    check(typeof summary === "string" && summary.trim() && summary.length <= 2000 &&
      Array.isArray(changes) && changes.length > 0 && changes.length <= HARNESS_LIMITS.files &&
      outputBytes({ summary, changes }) <= 1_800_000,
    "INVALID_PROPOSAL", "Expected a bounded source change proposal");
    check(typeof ctx.operationId === "string" && /^[a-zA-Z0-9_-]{1,120}$/.test(ctx.operationId),
      "INVALID_OPERATION", "Proposal requires a durable tool operation identity");
    const proposalId = sha256(`${job.id}:${ctx.operationId}`).slice(0, 32);
    const proposed = [], payload = [], names = new Set();
    let totalBytes = 0;
    for (const change of changes) {
      throwIfAborted(ctx.signal);
      check(change && typeof change === "object" && !Array.isArray(change) &&
        Object.keys(change).length === 3 && ["path", "expected_sha256", "content"].every((key) => Object.hasOwn(change, key)),
      "INVALID_PROPOSAL", "Expected exact path, expected_sha256 and content fields");
      sourcePath(change.path);
      const existing = entries.get(change.path);
      check(!names.has(change.path), "INVALID_PROPOSAL", "Duplicate proposed source path");
      names.add(change.path);
      check(!record.protectedPaths.includes(change.path), "HANDWRITTEN_PROTECTED", "This path retains a captured source lock");
      check(existing ? exactHash(change.expected_sha256) && change.expected_sha256 === existing.sha256 : change.expected_sha256 === null,
        "SOURCE_HASH_MISMATCH", "Proposal must reference the original frozen hash, or null for creation");
      check(change.content === null ? !!existing : typeof change.content === "string" && change.content.isWellFormed() && !change.content.includes("\0"),
        "INVALID_PROPOSAL", "Content must be UTF-8 text without NUL, or null to delete an existing file");
      if (existing) await readSnapshotBytes(snapshot.workspace, `source/${change.path}`, existing);
      const bytes = change.content === null ? null : Buffer.from(change.content, "utf8");
      if (bytes) {
        totalBytes += bytes.length;
        check(bytes.length <= HARNESS_LIMITS.fileBytes && totalBytes <= HARNESS_LIMITS.sourceBytes,
          "INVALID_PROPOSAL", "Proposed source exceeds its byte limit");
        check(!existing || sha256(bytes) !== existing.sha256, "EMPTY_SOURCE_CHANGE", "Proposed content already matches the original");
      }
      proposed.push({ path: change.path, expectedSha256: change.expected_sha256,
        sha256: bytes === null ? null : sha256(bytes), size: bytes?.length ?? 0, deleted: bytes === null });
      payload.push({ path: change.path, bytes });
    }
    // File-to-directory rewrites require a different native transaction. Reject
    // ambiguous path trees instead of letting staging order decide the result.
    for (const name of names) {
      const parts = name.split("/");
      for (let length = 1; length < parts.length; length++)
        check(!entries.has(parts.slice(0, length).join("/")) && !names.has(parts.slice(0, length).join("/")),
          "INVALID_PROPOSAL", "Proposed file conflicts with a file ancestor");
      check(![...entries.keys(), ...names].some((other) => other.startsWith(`${name}/`)),
        "INVALID_PROPOSAL", "Proposed file conflicts with a source directory");
    }
    proposed.sort((a, b) => compareSourcePaths(a.path, b.path));
    const review = { schemaVersion: 1, proposalId, jobId: job.id, projectId: harness.projectId,
      conversationId: harness.conversationId, sourceFingerprint: harness.source.fingerprint,
      summary, changes: proposed };
    review.digest = proposalDigest(review);
    let previous;
    try { previous = await this.#review(job.id, proposalId); }
    catch (error) { if (error.code !== "ENOENT") throw error; }
    if (previous) {
      check(previous.digest === review.digest, "OPERATION_MISMATCH", "Proposal operation was reused with different changes");
      await this.#verifyProposal(snapshot, previous);
      return this.#toolReview(previous, await this.#applied(job.id, previous));
    }
    for (const file of payload) {
      throwIfAborted(ctx.signal);
      if (file.bytes !== null) await writeHarnessBytes(control, `proposal-files/${proposalId}/${file.path}`, file.bytes);
    }
    throwIfAborted(ctx.signal);
    // Publication is a private review record, never a source write. A failed or
    // cancelled staging operation leaves no exportable proposal record.
    await writeHarnessRecord(control, reviewPath(proposalId), review);
    return this.#toolReview(review);
  }

  #toolReview(review, applied = null) {
    return { proposalId: review.proposalId, jobId: review.jobId, digest: review.digest,
      summary: review.summary, status: applied ? "applied" : "pending", changeCount: review.changes.length,
      changedPaths: review.changes.slice(0, 20).map((file) => file.path),
      changedPathsTruncated: review.changes.length > 20,
      originalSourceModified: !!applied, buildValidated: false, verificationState: "not_run" };
  }
  async #review(jobId, proposalId) {
    const control = await this.store.files(jobId);
    const review = await readHarnessRecord(control, reviewPath(proposalId));
    check(review.schemaVersion === 1 && review.jobId === jobId && review.proposalId === proposalId &&
      exactHash(review.digest) && proposalDigest(review) === review.digest && Array.isArray(review.changes) &&
      review.changes.length > 0 && review.changes.length <= HARNESS_LIMITS.files,
    "PROPOSAL_CHANGED", "The private review record changed");
    return review;
  }
  async #verifyProposal(snapshot, review, { verifySource = true } = {}) {
    check(review.projectId === snapshot.harness.projectId && review.conversationId === snapshot.harness.conversationId &&
      review.sourceFingerprint === snapshot.harness.source.fingerprint && snapshot.harness.mode === "execute",
    "PROPOSAL_SOURCE_MISMATCH", "Review is bound to a different native source");
    // Recheck all original files, not only changed files: native adoption is
    // based on this exact frozen project, and a corrupt snapshot cannot approve.
    if (verifySource) for (const entry of snapshot.record.manifest)
      await readSnapshotBytes(snapshot.workspace, `source/${entry.path}`, entry);
    const files = [];
    let previousPath;
    for (const change of review.changes) {
      sourcePath(change.path);
      const original = snapshot.entries.get(change.path);
      check((previousPath === undefined || compareSourcePaths(previousPath, change.path) < 0) &&
        !snapshot.record.protectedPaths.includes(change.path) &&
        (original ? change.expectedSha256 === original.sha256 : change.expectedSha256 === null) &&
        typeof change.deleted === "boolean" &&
        (change.deleted ? !!original && change.sha256 === null && change.size === 0 : exactHash(change.sha256)),
      "PROPOSAL_CHANGED", "Reviewed change differs from its frozen input");
      const bytes = change.deleted ? null : await readSnapshotBytes(snapshot.control,
        `proposal-files/${review.proposalId}/${change.path}`, change);
      if (bytes !== null) decodeText(bytes);
      files.push({ path: change.path, expectedSha256: change.expectedSha256, data: bytes?.toString("base64") ?? null, delete: change.deleted });
      previousPath = change.path;
    }
    return files;
  }
  async #applied(jobId, review) {
    let record;
    for (let attempt = 0; attempt < 3; attempt++) {
      try { record = await readHarnessRecord(await this.store.files(jobId), appliedPath(review.proposalId)); break; }
      catch (error) {
        if (error.code === "ENOENT") return null;
        // Atomic link publication briefly has its owned temporary link too.
        // Recheck a bounded number of times; persistent multiple links still
        // fail, and source-file reads never relax their link validation.
        if (error.code !== "UNSAFE_SOURCE" || attempt === 2) throw error;
        await new Promise((resolve) => setTimeout(resolve, 5));
      }
    }
    check(record.schemaVersion === 1 && record.jobId === jobId && record.proposalId === review.proposalId &&
      record.digest === review.digest && typeof record.appliedAt === "string" &&
      Number.isFinite(Date.parse(record.appliedAt)) && new Date(record.appliedAt).toISOString() === record.appliedAt,
    "APPLIED_RECEIPT_CHANGED", "The native application receipt differs from this review");
    return record;
  }

  // Host-only source construction grants no publication authority. Build tools
  // receive verified private bytes while native review retains its own gate.
  async buildSnapshot(ctx, { proposalId, expectedDigest } = {}) {
    const snapshot = await this.#context(ctx);
    const files = new Map();
    for (const entry of snapshot.record.manifest) {
      throwIfAborted(ctx.signal);
      files.set(entry.path, await readSnapshotBytes(snapshot.workspace, `source/${entry.path}`, entry));
    }
    check((proposalId === undefined) === (expectedDigest === undefined),
      "INVALID_BUILD_PROPOSAL", "A build overlay requires both proposal ID and exact digest");
    if (proposalId !== undefined) {
      const review = await this.#review(snapshot.job.id, proposalId);
      check(exactHash(expectedDigest) && review.digest === expectedDigest &&
        snapshot.job.operations.some((operation) => operation.status === "completed" &&
          ["harness.propose_changes", "pixel.propose", "model.propose"].includes(operation.name) &&
          operation.result?.proposalId === proposalId && operation.result.digest === expectedDigest),
      "BUILD_PROPOSAL_CHANGED", "Build overlay must match a completed proposal operation from this task");
      // A running task can compile its private proposal before final review.
      // Export/adoption keep their completed-job gate and separate approval.
      for (const change of await this.#verifyProposal(snapshot, review, { verifySource: false })) {
        if (change.delete) files.delete(change.path);
        else files.set(change.path, Buffer.from(change.data, "base64"));
      }
    }
    const totalBytes = [...files.values()].reduce((total, bytes) => total + bytes.length, 0);
    check(files.size <= HARNESS_LIMITS.files && totalBytes <= HARNESS_LIMITS.sourceBytes,
      "BUILD_SOURCE_LIMIT", "Final text build overlay exceeds its captured source budget");
    return { job: snapshot.job, files, proposalId: proposalId ?? null, proposalDigest: expectedDigest ?? null };
  }
  // These methods are intentionally absent from the tool registry. Native
  // review actions supply the exact digest the user saw and target ownership.
  async getReview(jobId, proposalId) {
    const snapshot = await this.snapshot(jobId);
    const review = await this.#review(jobId, proposalId);
    await this.#verifyProposal(snapshot, review);
    const applied = await this.#applied(jobId, review);
    return { ...review, status: applied ? "applied" : "pending", appliedAt: applied?.appliedAt ?? null,
      originalSourceModified: !!applied, buildValidated: false };
  }
  async listReviews(jobId) {
    const snapshot = await this.snapshot(jobId);
    const operations = snapshot.job.operations.filter((operation) =>
      ["harness.propose_changes", "pixel.propose", "model.propose"].includes(operation.name) && operation.status === "completed");
    const seen = new Set(), proposals = [];
    let truncated = false;
    // One complete original check covers every displayed review for this job.
    for (const entry of snapshot.record.manifest)
      await readSnapshotBytes(snapshot.workspace, `source/${entry.path}`, entry);
    for (const operation of operations) {
      const id = operation.result?.proposalId;
      if (seen.has(id)) continue;
      seen.add(id);
      const review = await this.#review(jobId, id);
      check(operation.result.digest === review.digest, "PROPOSAL_CHANGED", "Review differs from its durable tool receipt");
      const files = await this.#verifyProposal(snapshot, review, { verifySource: false });
      const applied = await this.#applied(jobId, review);
      const projection = { id, digest: review.digest, status: applied ? "applied" : "pending", summary: review.summary,
        appliedAt: applied?.appliedAt ?? null, changeCount: review.changes.length, changes: [], changesTruncated: false };
      if (outputBytes([...proposals, projection]) > 510_000) { truncated = true; break; }
      for (let index = 0; index < review.changes.length; index++) {
        const change = review.changes[index], original = snapshot.entries.get(change.path);
        const beforeText = original ? decodeText(await readSnapshotBytes(snapshot.workspace, `source/${change.path}`, original)) : null;
        const afterText = change.deleted ? null : decodeText(Buffer.from(files[index].data, "base64"));
        const remaining = Math.max(0, 500_000 - outputBytes([...proposals, projection]) - 2000);
        const maximum = Math.min(24_000, Math.floor(remaining / 2));
        const before = beforeText === null ? null : textPreview(beforeText, maximum);
        const after = afterText === null ? null : textPreview(afterText, maximum);
        const displayed = { path: change.path, kind: change.deleted ? "delete" : original ? "modify" : "create",
          beforeSha256: change.expectedSha256, afterSha256: change.sha256,
          before: before?.text ?? null, after: after?.text ?? null,
          beforeTruncated: before?.truncated ?? false, afterTruncated: after?.truncated ?? false };
        projection.changes.push(displayed);
        if (outputBytes([...proposals, projection]) > 510_000) {
          projection.changes.pop(); projection.changesTruncated = true; truncated = true; break;
        }
        truncated ||= displayed.beforeTruncated || displayed.afterTruncated;
      }
      proposals.push(projection);
    }
    return { proposals, total: new Set(operations.map((operation) => operation.result?.proposalId)).size, truncated };
  }
  async exportProposal(jobId, { proposalId, expectedDigest }) {
    const snapshot = await this.snapshot(jobId);
    check(snapshot.job.status === "completed",
      "PROPOSAL_NOT_READY", "Only a successfully completed job can export its review");
    const review = await this.#review(jobId, proposalId);
    check(exactHash(expectedDigest) && expectedDigest === review.digest,
      "STALE_APPROVAL", "Export requires the exact authoritative review digest");
    const files = await this.#verifyProposal(snapshot, review);
    return { jobId, proposalId, digest: review.digest, projectId: review.projectId,
      conversationId: review.conversationId, sourceFingerprint: review.sourceFingerprint, files };
  }
  async markApplied(jobId, { proposalId, expectedDigest }) {
    const snapshot = await this.snapshot(jobId);
    check(snapshot.job.status === "completed", "PROPOSAL_NOT_READY", "Only a completed job can record native application");
    const review = await this.#review(jobId, proposalId);
    check(exactHash(expectedDigest) && expectedDigest === review.digest,
      "STALE_APPROVAL", "Application requires the exact reviewed digest");
    await this.#verifyProposal(snapshot, review);
    let applied = await this.#applied(jobId, review);
    if (!applied) {
      // Rust calls this only after its guarded native transaction commits. This
      // receipt is separate from immutable proposal data; re-reading an applied
      // review still verifies its original frozen source and proposed bytes.
      applied = { schemaVersion: 1, jobId, proposalId, digest: review.digest, appliedAt: new Date().toISOString() };
      let published;
      try { published = await writeHarnessRecord(snapshot.control, appliedPath(proposalId), applied); }
      catch (error) {
        // Two host retries can reach publication together with different times.
        // An already published receipt is authoritative only for this digest.
        const existing = await this.#applied(jobId, review);
        if (!existing) throw error;
        applied = existing; published = false;
      }
      if (published) {
        await this.store.save(snapshot.job);
        await this.store.event(jobId, "harness.proposal.applied", { proposalId, digest: review.digest });
      }
    }
    return { proposalId, digest: review.digest, status: "applied", appliedAt: applied.appliedAt };
  }
}
