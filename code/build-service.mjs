/** Build attempts belong to the running Harness operation and its existing job
 * lease. This service never reacquires that lease or writes job.json: runtime
 * owns the operation receipt, while this service owns attempt/log/artifact data.
 * A reserved attempt survives crashes and is never silently executed twice. */
import fs from "node:fs/promises";
import { constants } from "node:fs";
import path from "node:path";
import { randomUUID } from "node:crypto";
import { check, cloneJson, throwIfAborted } from "../experimental/runtime/src/errors.mjs";
import { Workspace } from "../experimental/runtime/src/workspace.mjs";
import { BUILD_LIMITS, captureBuildSource } from "./build-snapshot.mjs";
import { sha256, recordDigest, compareSourcePaths, writeHarnessBuildBytes } from "./harness-snapshot.mjs";
import { NativeBuildRunner } from "./build-runner.mjs";

const uuid = (value) => typeof value === "string" && /^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/.test(value);
const STATES = new Set(["running", "completed", "failed", "cancelled", "timed_out", "output_limit", "blocked", "interrupted"]);
const indexPath = "control/build-runs/index.json";
const recordPath = (runId) => {
  check(uuid(runId), "INVALID_BUILD_ID", "Expected a build run ID");
  return `control/build-runs/${runId}.json`;
};
const publicError = (code, message) => ({ code, message: String(message).slice(0, 1000) });
function logExcerpt(text, maximum = 6000) {
  const tail = text.slice(-maximum);
  // JSON control escaping can expand a short log beyond the tool result budget.
  return Buffer.byteLength(JSON.stringify(tail)) <= 12_000 ? tail : tail.slice(-1500);
}

export class HarnessBuildService {
  #store; #harness; #runner; #active = new Map(); #starting = new Set(); #closed = false;
  constructor({ store, harnessService, runner = new NativeBuildRunner() }) {
    check(store && harnessService && typeof runner.run === "function" && typeof runner.shutdown === "function",
      "INVALID_BUILD_HOST", "Expected private task store, Harness snapshot service and native runner");
    this.#store = store; this.#harness = harnessService; this.#runner = runner;
  }
  hasActiveWork() { return this.#active.size !== 0 || this.#starting.size !== 0 || this.#runner.hasActiveWork?.() === true; }
  async #index(control) {
    try {
      const index = JSON.parse((await control.read(indexPath)).content);
      check(index.schemaVersion === 1 && Array.isArray(index.runs) && index.runs.length <= BUILD_LIMITS.attempts &&
        new Set(index.runs.map((entry) => entry.runId)).size === index.runs.length &&
        index.runs.every((entry) => uuid(entry.runId) && typeof entry.operationId === "string" &&
          /^[a-zA-Z0-9_-]{1,120}$/.test(entry.operationId) && /^[a-f0-9]{64}$/.test(entry.binding)),
      "BUILD_RECORD_CHANGED", "Private build attempt index changed");
      return index;
    } catch (error) { if (error.code !== "ENOENT") throw error; return { schemaVersion: 1, runs: [] }; }
  }
  async #save(control, record) {
    record.revision++;
    await control.write(recordPath(record.runId), JSON.stringify(record));
  }
  async #record(jobId, runId) {
    const record = JSON.parse((await (await this.#store.files(jobId)).read(recordPath(runId))).content);
    check(record.schemaVersion === 1 && record.jobId === jobId && record.runId === runId && STATES.has(record.status) &&
      Number.isSafeInteger(record.logBytes) && record.logBytes >= 0 && record.logBytes <= BUILD_LIMITS.outputBytes &&
      Array.isArray(record.artifacts) && record.artifacts.length <= BUILD_LIMITS.artifacts,
    "BUILD_RECORD_CHANGED", "Private build record identity or bounds changed");
    return record;
  }
  #view(record) {
    const { schemaVersion, operationId, binding, ...view } = record;
    if (view.status === "running" && !this.#active.has(record.runId)) {
      view.status = "interrupted";
      view.error = publicError("BUILD_OWNER_LOST", "The previous build owner stopped; this attempt will not be executed again automatically.");
    }
    if (view.artifacts.length > 20) { view.artifacts = view.artifacts.slice(0, 20); view.artifactTruncated = true; }
    return cloneJson(view);
  }
  async listForJob(job) {
    if (job.profile !== "harness") return [];
    const control = await this.#store.files(job.id), index = await this.#index(control);
    const runs = [];
    for (const entry of index.runs) runs.push(this.#view(await this.#record(job.id, entry.runId)));
    return runs;
  }
  run(ctx, args) {
    const pending = this.#run(ctx, args); this.#starting.add(pending);
    const settled = () => this.#starting.delete(pending); pending.then(settled, settled);
    return pending;
  }
  async #run(ctx, args) {
    throwIfAborted(ctx.signal);
    check(!this.#closed, "BUILD_HOST_CLOSED", "Build service is closing");
    check(["javac", "gradle"].includes(args.recipe) && ["classes", "build", "test"].includes(args.target),
      "INVALID_BUILD_RECIPE", "Select a fixed build recipe and target");
    check(typeof ctx.operationId === "string" && /^[a-zA-Z0-9_-]{1,120}$/.test(ctx.operationId),
      "INVALID_OPERATION", "Build needs a durable tool operation identity");
    const source = await captureBuildSource(this.#harness, ctx, args);
    check(source.job.input.harness.mode === "execute" && source.policy.enabled === true,
      "BUILD_NOT_AUTHORIZED", "Enable controlled builds before creating an Execute task");
    check(source.job.operations.some((operation) => operation.name === "harness.report_plan" && operation.status === "completed"),
      "PLAN_REQUIRED", "Record a plan before running captured source code");
    const binding = recordDigest({ args, sourceFingerprint: source.fingerprint, policy: source.policy });
    const control = await this.#store.files(ctx.jobId), index = await this.#index(control);
    const existing = index.runs.find((entry) => entry.operationId === ctx.operationId);
    if (existing) {
      check(existing.binding === binding, "BUILD_REQUEST_CONFLICT", "Build operation identity belongs to different frozen inputs");
      const record = await this.#record(ctx.jobId, existing.runId);
      check(record.status !== "running", "BUILD_INTERRUPTED", "A previously reserved build cannot execute again without reconciliation");
      return this.#result(record);
    }
    check(index.runs.length < BUILD_LIMITS.attempts && ![...this.#active.values()].some((run) => run.jobId === ctx.jobId),
      "BUILD_LIMIT", "Task build capacity or attempt limit reached");
    throwIfAborted(ctx.signal);
    check(!this.#closed, "BUILD_HOST_CLOSED", "Shutdown cancelled pending build admission");
    const runId = randomUUID(), attemptRoot = path.join(this.#store.directory(ctx.jobId), "build-runs", runId);
    const attempt = await new Workspace(attemptRoot).init();
    await fs.mkdir(path.join(attempt.root, "source"), { mode: 0o700 });
    await fs.mkdir(path.join(attempt.root, "home"), { mode: 0o700 });
    for (const [name, bytes] of source.files) {
      throwIfAborted(ctx.signal);
      await writeHarnessBuildBytes(attempt, `source/${name}`, bytes);
    }
    const logPath = await attempt.resolve("log.txt");
    const log = await fs.open(logPath, constants.O_CREAT | constants.O_EXCL | constants.O_WRONLY | constants.O_NOFOLLOW, 0o600);
    const record = { schemaVersion: 1, revision: 0, jobId: ctx.jobId, operationId: ctx.operationId, binding, runId,
      recipe: args.recipe, target: args.target, status: "running", sourceFingerprint: source.fingerprint,
      proposalId: source.proposalId, proposalDigest: source.proposalDigest,
      startedAt: new Date().toISOString(), finishedAt: null, exitCode: null, signal: null, error: null,
      logBytes: 0, logTruncated: false, artifactCount: 0, artifactTruncated: false, artifacts: [], actualGeneratedCodeExecuted: false };
    const owner = { jobId: ctx.jobId, controller: new AbortController(), promise: null, tail: "" };
    this.#active.set(runId, owner);
    try {
      await this.#save(control, record);
      index.runs.push({ runId, operationId: ctx.operationId, binding });
      await control.write(indexPath, JSON.stringify(index));
      const signal = ctx.signal ? AbortSignal.any([ctx.signal, owner.controller.signal]) : owner.controller.signal;
      if (this.#closed) owner.controller.abort();
      owner.promise = this.#execute(ctx, source, attempt, control, record, owner, log, signal);
      return await owner.promise;
    } finally { await log.close(); this.#active.delete(runId); }
  }
  async #execute(ctx, source, attempt, control, record, owner, log, signal) {
    let outcome;
    try {
      outcome = await this.#runner.run({ schemaVersion: 1, runId: record.runId, root: attempt.root,
        recipe: record.recipe, target: record.target, network: source.policy.network,
        timeoutMs: BUILD_LIMITS.timeoutMs, maxOutputBytes: BUILD_LIMITS.outputBytes }, { signal,
        onFrame: async (frame) => {
          if (frame.type === "started") record.actualGeneratedCodeExecuted = true;
          if (frame.type === "output") {
            const bytes = Buffer.from(frame.text);
            check(record.logBytes + bytes.length <= BUILD_LIMITS.outputBytes, "BUILD_OUTPUT_LIMIT", "Build log exceeded its captured budget");
            let written = 0;
            while (written < bytes.length) {
              const part = await log.write(bytes, written, bytes.length - written);
              check(part.bytesWritten > 0, "BUILD_LOG_WRITE_FAILED", "Private build log write made no progress");
              written += part.bytesWritten;
            }
            record.logBytes += bytes.length;
            owner.tail = (owner.tail + frame.text).slice(-6000);
          }
          await this.#save(control, record);
        } });
      if (outcome.status === "completed") {
        // Compilation writes into its copy. Recheck the immutable input and
        // proposal instead of invalidating valid outputs changed by a compiler.
        const verified = await captureBuildSource(this.#harness, { ...ctx, signal: undefined }, { recipe: record.recipe,
          target: record.target, ...(record.proposalId ? { proposalId: record.proposalId, expectedDigest: record.proposalDigest } : {}) });
        check(verified.fingerprint === source.fingerprint, "BUILD_SOURCE_CHANGED", "Frozen build input changed while the helper was running");
        const artifacts = await this.#collectArtifacts(attempt, record.recipe);
        record.artifacts = artifacts.files; record.artifactCount = artifacts.files.length; record.artifactTruncated = artifacts.truncated;
      }
    } catch (error) {
      outcome = { status: signal.aborted ? "cancelled" : "failed", exitCode: null, signal: null,
        error: publicError(error.code ?? "BUILD_HOST_ERROR", error.message) };
    }
    if (signal.aborted && outcome.status === "completed") outcome.status = "cancelled";
    Object.assign(record, outcome, { finishedAt: new Date().toISOString(), logTruncated: outcome.status === "output_limit" });
    await log.sync(); await this.#save(control, record);
    await this.#store.event(ctx.jobId, "build.finished", { runId: record.runId, status: record.status,
      recipe: record.recipe, target: record.target, artifactCount: record.artifactCount });
    return { ...this.#view(record), logTail: logExcerpt(owner.tail) };
  }
  async #collectArtifacts(attempt, recipe) {
    const sourceRoot = path.join(attempt.root, "source"), pending = [{ directory: sourceRoot, relative: "", depth: 0 }];
    const files = []; let scanned = 0, totalBytes = 0, truncated = false;
    while (pending.length) {
      const current = pending.pop();
      const names = (await fs.readdir(current.directory)).sort(compareSourcePaths);
      for (const name of names) {
        if (++scanned > 12_000) { truncated = true; break; }
        const relative = current.relative ? `${current.relative}/${name}` : name;
        const filePath = path.join(current.directory, name), stat = await fs.lstat(filePath);
        if (stat.isDirectory()) {
          if (current.depth < 12) pending.push({ directory: filePath, relative, depth: current.depth + 1 });
          else truncated = true;
          continue;
        }
        const eligible = recipe === "javac" ? relative.startsWith(".rh-build/classes/") && relative.endsWith(".class")
          : /(?:^|\/)build\/libs\/[^/]+\.jar$/.test(relative);
        if (!eligible) continue;
        check(stat.isFile() && !stat.isSymbolicLink() && stat.nlink === 1, "BUILD_ARTIFACT_UNSAFE", "Compiler artifact is not a private regular file");
        if (Buffer.byteLength(relative) > 512 || /[\\:\x00-\x1f\x7f-\x9f]/.test(relative) || files.length >= BUILD_LIMITS.artifacts || stat.size > BUILD_LIMITS.artifactBytes ||
          totalBytes + stat.size > BUILD_LIMITS.artifactTotalBytes) { truncated = true; continue; }
        const handle = await fs.open(filePath, constants.O_RDONLY | constants.O_NOFOLLOW);
        let bytes;
        try {
          const checked = await handle.stat();
          check(checked.isFile() && checked.nlink === 1 && checked.size === stat.size,
            "BUILD_ARTIFACT_CHANGED", "Compiler artifact changed during collection");
          const buffer = Buffer.allocUnsafe(checked.size + 1); let length = 0;
          while (length < buffer.length) {
            const read = await handle.read(buffer, length, buffer.length - length, length);
            if (!read.bytesRead) break; length += read.bytesRead;
          }
          check(length === checked.size, "BUILD_ARTIFACT_CHANGED", "Compiler artifact size changed during collection");
          bytes = buffer.subarray(0, length);
        } finally { await handle.close(); }
        const artifactPath = recipe === "javac" ? `classes/${relative.slice(".rh-build/classes/".length)}` : relative;
        const target = await attempt.resolve(`artifacts/${artifactPath}`, { parents: true });
        await fs.writeFile(target, bytes, { flag: "wx", mode: 0o600 });
        files.push({ path: artifactPath, size: bytes.length, sha256: sha256(bytes) }); totalBytes += bytes.length;
      }
      if (scanned > 12_000) break;
    }
    files.sort((a, b) => compareSourcePaths(a.path, b.path));
    return { files, truncated };
  }
  async #result(record) {
    const page = await this.readLog({ jobId: record.jobId }, { runId: record.runId,
      offset: Math.max(0, record.logBytes - 6000), limit: 6000 });
    return { ...this.#view(record), logTail: logExcerpt(page.content) };
  }
  async readLog(ctx, { runId, offset = 0, limit = 8000 }) {
    throwIfAborted(ctx.signal);
    check(uuid(runId) && Number.isSafeInteger(offset) && offset >= 0 && offset <= BUILD_LIMITS.outputBytes &&
      Number.isSafeInteger(limit) && limit >= 1 && limit <= 16_000,
    "INVALID_BUILD_LOG_RANGE", "Build log pages use byte offsets and at most 16000 bytes");
    const record = await this.#record(ctx.jobId, runId);
    const control = await this.#store.files(ctx.jobId), target = await control.resolve(`build-runs/${runId}/log.txt`);
    const handle = await fs.open(target, constants.O_RDONLY | constants.O_NOFOLLOW);
    try {
      const stat = await handle.stat();
      check(stat.isFile() && stat.nlink === 1 && stat.size <= BUILD_LIMITS.outputBytes,
        "BUILD_LOG_CHANGED", "Private build log size or file identity changed");
      // The run receipt acknowledges bytes after append. Expose only that prefix
      // while a writer is active, keeping counts and the returned page coherent.
      const totalBytes = Math.min(stat.size, record.logBytes), start = Math.min(offset, totalBytes);
      const bytes = Buffer.alloc(Math.min(limit + 8, totalBytes - start));
      const read = await handle.read(bytes, 0, bytes.length, start);
      let from = 0, to = Math.min(read.bytesRead, limit);
      const continuation = (value) => (value & 0xc0) === 0x80;
      while (from < read.bytesRead && continuation(bytes[from])) from++;
      while (to > from && to < read.bytesRead && continuation(bytes[to])) to--;
      if (to === from && from < read.bytesRead) {
        to = from + 1; while (to < read.bytesRead && continuation(bytes[to])) to++;
      }
      if (Buffer.byteLength(JSON.stringify(bytes.subarray(from, to).toString("utf8"))) > 48_000) {
        let low = from, high = to;
        while (low < high) {
          const end = Math.ceil((low + high) / 2);
          if (Buffer.byteLength(JSON.stringify(bytes.subarray(from, end).toString("utf8"))) <= 48_000) low = end;
          else high = end - 1;
        }
        to = low;
        while (to > from && to < read.bytesRead && continuation(bytes[to])) to--;
      }
      const next = start + to;
      return { content: bytes.subarray(from, to).toString("utf8"), nextOffset: next < totalBytes ? next : null,
        truncated: start + from > 0 || next < totalBytes, totalBytes };
    } finally { await handle.close(); }
  }
  async artifactDirectory(jobId, runId) {
    const record = await this.#record(jobId, runId);
    check(record.status === "completed" && record.artifactCount > 0,
      "BUILD_ARTIFACT_UNAVAILABLE", "This run has no completed collected artifacts");
    const directory = path.join(this.#store.directory(jobId), "build-runs", runId, "artifacts");
    const stat = await fs.lstat(directory);
    check(stat.isDirectory() && !stat.isSymbolicLink(), "BUILD_ARTIFACT_UNSAFE", "Collected artifact directory is unavailable");
    const workspace = await new Workspace(directory).init();
    for (const artifact of record.artifacts) {
      check(Number.isSafeInteger(artifact.size) && artifact.size >= 0 && artifact.size <= BUILD_LIMITS.artifactBytes,
        "BUILD_ARTIFACT_CHANGED", "Collected artifact metadata exceeded its limit");
      const target = await workspace.resolve(artifact.path), handle = await fs.open(target, constants.O_RDONLY | constants.O_NOFOLLOW);
      try {
        const stat = await handle.stat();
        check(stat.isFile() && stat.nlink === 1 && stat.size === artifact.size,
          "BUILD_ARTIFACT_CHANGED", "Collected artifact file identity or size changed");
        const buffer = Buffer.allocUnsafe(stat.size + 1); let length = 0;
        while (length < buffer.length) {
          const read = await handle.read(buffer, length, buffer.length - length, length);
          if (!read.bytesRead) break; length += read.bytesRead;
        }
        check(length === artifact.size && sha256(buffer.subarray(0, length)) === artifact.sha256,
          "BUILD_ARTIFACT_CHANGED", "Collected artifact bytes differ from the build receipt");
      } finally { await handle.close(); }
    }
    return { directory: workspace.root, runId, artifactCount: record.artifactCount };
  }
  async shutdown() {
    this.#closed = true;
    for (const owner of this.#active.values()) owner.controller.abort();
    await this.#runner.shutdown(); await Promise.allSettled([...this.#starting]);
  }
}
