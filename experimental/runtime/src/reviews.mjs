import * as fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
  createHash,
  randomBytes,
  randomUUID,
  timingSafeEqual,
} from "node:crypto";
import {
  captureSourceArtifact,
  recordDigest,
  sha256,
} from "./artifact-snapshot.mjs";
import { Workspace } from "./workspace.mjs";
import { SubprocessSupervisor } from "./subprocesses.mjs";
import { check, cloneJson, RuntimeError, throwIfAborted } from "./errors.mjs";

const UUID = /^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/;
const HEX = /^[a-f0-9]{64}$/;
const canonical = (value) =>
  Array.isArray(value)
    ? `[${value.map(canonical).join(",")}]`
    : value && typeof value === "object"
      ? `{${Object.keys(value)
          .sort()
          .map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`)
          .join(",")}}`
      : JSON.stringify(value);
const digest = (value) =>
  createHash("sha256").update(canonical(value)).digest("hex");
const hashToken = (value) => createHash("sha256").update(value).digest("hex");
const exactHex = (value) =>
  typeof value === "string" && value.length === 64 && HEX.test(value);
function reviewDigest(record) {
  return digest({
    schemaVersion: record.schemaVersion,
    id: record.id,
    jobId: record.jobId,
    kind: record.kind,
    source: record.source,
    payload: record.payload,
    preview: record.preview,
  });
}
function bounded(value, maxBytes = 64_000) {
  return cloneJson(value, { maxBytes, code: "REVIEW_SIZE_LIMIT" });
}
function sourceKey(value) {
  check(
    typeof value === "string" && /^[a-zA-Z0-9_-]{1,80}(?![\s\S])/.test(value),
    "INVALID_SOURCE_ID",
    "Invalid host source ID",
  );
  return value;
}

/** Host-owned review/approval service. Never expose approve/apply to agent tools. */
export class HostReviewService {
  #store;
  #supervisor;
  #now;
  #ttl;
  constructor({
    store,
    pythonExecutable = "/usr/bin/python3",
    now = () => Date.now(),
    approvalTtlMs = 900_000,
  }) {
    check(
      store && path.isAbsolute(pythonExecutable),
      "INVALID_REVIEW_HOST",
      "Use the job store and an absolute host interpreter",
    );
    check(
      Number.isInteger(approvalTtlMs) &&
        approvalTtlMs > 0 &&
        approvalTtlMs <= 3_600_000,
      "INVALID_REVIEW_HOST",
      "Approval TTL must be 1 ms–1 hour",
    );
    this.#store = store;
    this.#now = now;
    this.#ttl = approvalTtlMs;
    const script = fileURLToPath(
      new URL("./adapters/python_bridge.py", import.meta.url),
    );
    const names = [
      "maker.preview_revision",
      "maker.prepare_revision",
      "maker.preview_source_edit",
      "maker.prepare_source_edit",
      "porter.import_snapshot",
      "porter.create_patch",
      "porter.apply_copy",
      "porter.propose_metadata",
      "porter.propose_identifier",
    ];
    this.#supervisor = new SubprocessSupervisor({
      trustedCommands: Object.fromEntries(
        names.map((name) => [
          name,
          { executable: pythonExecutable, args: ["-I", "-B", script, name] },
        ]),
      ),
    });
  }
  async shutdown() {
    await this.#supervisor.shutdown();
  }
  hasActiveWork() {
    return this.#supervisor.hasActiveWork();
  }
  async #control(jobId) {
    await this.#store.files(jobId);
    return new Workspace(
      path.join(this.#store.directory(jobId), "review-control"),
    ).init();
  }
  async #bridge(operation, args, workspace, signal) {
    const run = await this.#supervisor.run(operation, {
      cwd: workspace.root,
      signal,
      input: JSON.stringify(args),
      timeoutMs: 5000,
      maxOutputBytes: 256_000,
    });
    throwIfAborted(signal);
    let envelope;
    try {
      envelope = JSON.parse(run.stdout);
    } catch {
      throw new RuntimeError(
        "REVIEW_PROCESS_FAILED",
        `Trusted review bridge returned ${run.status}`,
      );
    }
    check(
      run.status === "completed" && envelope.ok,
      "REVIEW_REJECTED",
      envelope.error?.message ?? "Review operation failed",
    );
    return envelope.result;
  }
  async #exclusive(jobId, callback) {
    const release = await this.#store.acquire(jobId);
    try {
      return await callback(
        await this.#store.load(jobId),
        await this.#store.workspace(jobId),
      );
    } finally {
      await release();
    }
  }
  async #source(jobId, sourceId) {
    const source = JSON.parse(
      (
        await (
          await this.#control(jobId)
        ).read(`sources/${sourceKey(sourceId)}.json`)
      ).content,
    );
    check(
      source.jobId === jobId && source.id === sourceId,
      "WRONG_JOB",
      "Source belongs to another job",
    );
    return source;
  }
  async #load(jobId, reviewId) {
    check(
      typeof reviewId === "string" &&
        reviewId.length === 36 &&
        UUID.test(reviewId),
      "INVALID_REVIEW_ID",
      "Invalid review ID",
    );
    let record;
    try {
      record = JSON.parse(
        (await (await this.#control(jobId)).read(`reviews/${reviewId}.json`))
          .content,
      );
    } catch (error) {
      if (error.code === "ENOENT")
        throw new RuntimeError(
          "REVIEW_NOT_FOUND",
          "No review belongs to this job",
        );
      throw error;
    }
    check(
      record.jobId === jobId && record.id === reviewId,
      "WRONG_JOB",
      "Review belongs to another job",
    );
    check(
      record.schemaVersion === 1 &&
        ["maker_revision", "maker_source_edit", "porter_patch"].includes(
          record.kind,
        ) &&
        exactHex(record.digest) &&
        reviewDigest(record) === record.digest,
      "REVIEW_TAMPERED",
      "Authoritative review content changed",
    );
    return record;
  }
  async #save(record) {
    bounded(record, 240_000);
    await (
      await this.#control(record.jobId)
    ).write(`reviews/${record.id}.json`, JSON.stringify(record));
  }
  #public(record) {
    const expiresAt = record.approval?.expiresAt ?? null;
    const status =
      record.status === "approved" && expiresAt <= this.#now()
        ? "expired"
        : record.status;
    return bounded(
      {
        schemaVersion: 1,
        reviewId: record.id,
        jobId: record.jobId,
        kind: record.kind,
        status,
        reviewDigest: record.digest,
        preview: record.preview,
        verificationState: "not_run",
        expiresAt,
      },
      128_000,
    );
  }
  async #create(jobId, kind, source, payload, preview) {
    const record = {
      schemaVersion: 1,
      id: randomUUID(),
      jobId,
      kind,
      source,
      payload: bounded(payload),
      preview: bounded(preview),
      status: "pending",
      createdAt: new Date(this.#now()).toISOString(),
      approval: null,
    };
    record.digest = reviewDigest(record);
    await this.#save(record);
    await this.#store.event(jobId, "review.created", {
      reviewId: record.id,
      kind,
    });
    return {
      reviewId: record.id,
      reviewDigest: record.digest,
      kind,
      status: "awaiting_host_review",
      changedFiles:
        preview.modified ?? preview.changes?.map((change) => change.path) ?? [],
      verificationState: "not_run",
    };
  }
  async #makerSource(job, sourceOperationId) {
    const receipt = job.operations.find(
      (op) =>
        op.id === sourceOperationId &&
        ["maker.generate", "maker.revise", "maker.edit_source"].includes(
          op.name,
        ) &&
        op.status === "completed",
    );
    const generated =
      receipt?.name === "maker.generate" &&
      receipt.result?.status === "source_generated_only";
    const revised =
      ["maker.revise", "maker.edit_source"].includes(receipt?.name) &&
      receipt.result?.status === "reviewed_copy_created" &&
      typeof receipt.result.reviewId === "string" &&
      receipt.result.reviewId.length === 36 &&
      UUID.test(receipt.result.reviewId) &&
      sourceOperationId === `host-${receipt.result.reviewId}`;
    check(
      generated || revised,
      "INVALID_SOURCE",
      "Select a completed Maker source artifact from this job",
    );
    const expected = generated
      ? `generated/${sourceOperationId}`
      : `reviewed/${receipt.result.reviewId}`;
    check(
      receipt.result.outputDirectory === expected,
      "INVALID_SOURCE",
      "Source directory does not match its host receipt",
    );
    const workspace = await this.#store.workspace(job.id);
    await workspace.resolve(
      `${receipt.result.outputDirectory}/creator-spec.json`,
    );
    return path.join(workspace.root, receipt.result.outputDirectory);
  }
  // Called only by our registered preview tool while AgentRuntime owns the lease.
  async previewMakerFromTool(ctx, { sourceOperationId, newSpec }) {
    const job = await this.#store.load(ctx.jobId);
    check(
      job.profile === "maker",
      "WRONG_WORKFLOW",
      "Maker review requires a Maker job",
    );
    const sourceRoot = await this.#makerSource(job, sourceOperationId);
    const preview = await this.#bridge(
      "maker.preview_revision",
      { sourceRoot, newSpec: bounded(newSpec) },
      ctx.workspace,
      ctx.signal,
    );
    return this.#create(
      job.id,
      "maker_revision",
      { operationId: sourceOperationId, revision: preview.revision },
      { newSpec },
      preview,
    );
  }
  async previewMakerEditFromTool(ctx, { sourceOperationId, request }) {
    const job = await this.#store.load(ctx.jobId);
    check(
      job.profile === "maker",
      "WRONG_WORKFLOW",
      "Source editing requires a Maker job",
    );
    await captureSourceArtifact(this.#store, job, sourceOperationId);
    const sourceRoot = await this.#makerSource(job, sourceOperationId);
    const preview = await this.#bridge(
      "maker.preview_source_edit",
      { sourceRoot, request: bounded(request, 48_000) },
      ctx.workspace,
      ctx.signal,
    );
    return this.#create(
      job.id,
      "maker_source_edit",
      { operationId: sourceOperationId, revision: preview.revision },
      { request },
      preview,
    );
  }
  async requestMakerSourceEdit(jobId, { sourceOperationId, request }) {
    return this.#exclusive(jobId, async (job, workspace) => {
      const result = await this.previewMakerEditFromTool(
        { jobId, workspace },
        { sourceOperationId, request },
      );
      await this.#store.save(job);
      return result;
    });
  }
  async requestPorterPatch(jobId, args) {
    return this.#exclusive(jobId, async (job, workspace) => {
      const result = await this.previewPorterFromTool(
        { jobId, workspace },
        args,
      );
      await this.#store.save(job);
      return result;
    });
  }
  async getPorterSource(jobId) {
    const job = await this.#store.load(jobId);
    check(
      job.profile === "porter",
      "WRONG_WORKFLOW",
      "Porter source requires a Porter job",
    );
    let source;
    try {
      source = await this.#source(jobId, "primary");
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
      // Analysis-only jobs have no write grants. Their frozen input remains
      // readable after restart; current renderer drafts are never the baseline.
      source = { files: job.input.files, permittedPaths: [] };
    }
    return bounded(
      {
        files: source.files,
        permittedPaths: source.permittedPaths,
        targetId: job.input.targetId,
        identifierProfile: job.input.identifierProfile ?? null,
      },
      64_000,
    );
  }
  async requestPorterRecipe(jobId, recipe) {
    return this.#exclusive(jobId, async (job, workspace) => {
      const result = await this.previewPorterRecipeFromTool(
        { jobId, workspace },
        recipe,
      );
      await this.#store.save(job);
      return result;
    });
  }
  async previewPorterRecipeFromTool(ctx, recipe) {
    check(
      ["metadata", "identifier"].includes(recipe),
      "INVALID_RECIPE",
      "Unknown migration recipe",
    );
    const job = await this.#store.load(ctx.jobId);
    check(
      job.profile === "porter",
      "WRONG_WORKFLOW",
      "Porter recipe requires a Porter job",
    );
    const source = await this.#source(job.id, "primary");
    const result = await this.#bridge(
      `porter.propose_${recipe}`,
      {
        files: source.files,
        permittedPaths: source.permittedPaths,
        jobId: job.id,
        targetId: job.input.targetId,
        rights: job.input.rights,
        acknowledgeBeta: job.input.acknowledgeBeta,
        identifierProfile: job.input.identifierProfile,
      },
      ctx.workspace,
      ctx.signal,
    );
    if (result.proposal) {
      // Do not reconstruct a generic text patch here: doing so discards the
      // recipe's TODOs, version profile and validation limits before approval.
      result.review = await this.#create(
        job.id,
        "porter_patch",
        {
          id: "primary",
          revision: source.revision,
          fingerprint: source.fingerprint,
        },
        {},
        result.proposal,
      );
    }
    return result;
  }
  // Read-only host binding check used by bounded repair coordination. No approval.
  async assertRepairSource(jobId, reviewId, sourceOperationId) {
    const job = await this.#store.load(jobId);
    const record = await this.#load(jobId, reviewId);
    const snapshot = await captureSourceArtifact(
      this.#store,
      job,
      sourceOperationId,
    );
    if (record.kind !== "porter_patch") {
      check(
        job.profile === "maker" &&
          record.source.operationId === sourceOperationId,
        "REPAIR_SOURCE_MISMATCH",
        "Review targets a different Maker source artifact",
      );
    } else {
      check(
        job.profile === "porter",
        "WRONG_WORKFLOW",
        "Porter review requires a Porter job",
      );
      const source = await this.#source(jobId, record.source.id);
      check(
        source.revision === record.source.revision &&
          source.fingerprint === record.source.fingerprint,
        "STALE_REVIEW",
        "Imported source changed since review",
      );
      const inventory = Object.entries(source.files)
        .map(([name, text]) => ({
          path: name,
          bytes: Buffer.byteLength(text),
          sha256: sha256(text),
        }))
        .sort((a, b) => a.path.localeCompare(b.path));
      check(
        recordDigest(inventory) === recordDigest(snapshot.inventory),
        "REPAIR_SOURCE_MISMATCH",
        "Review input differs from the failed source artifact",
      );
    }
    return {
      sourceFingerprint: snapshot.fingerprint,
      reviewDigest: record.digest,
    };
  }
  async previewPorterFromTool(ctx, { sourceId, replacements, purpose }) {
    const job = await this.#store.load(ctx.jobId);
    check(
      job.profile === "porter",
      "WRONG_WORKFLOW",
      "Porter review requires a Porter job",
    );
    check(
      ["owner", "permission", "license-reviewed"].includes(job.input.rights),
      "RIGHTS_REQUIRED",
      "Declare source modification rights before proposing a patch",
    );
    const catalog = JSON.parse(
      await fs.readFile(
        new URL("../vendor/porter/catalog.json", import.meta.url),
        "utf8",
      ),
    );
    const target = catalog.targets.find((t) => t.id === job.input.targetId);
    check(
      target &&
        (target.channel !== "beta" || job.input.acknowledgeBeta === true),
      "TARGET_REVIEW_REQUIRED",
      "Choose a supported target and explicitly accept beta status before proposing a patch",
    );
    const source = await this.#source(job.id, sourceId);
    const preview = await this.#bridge(
      "porter.create_patch",
      {
        files: source.files,
        replacements: bounded(replacements, 24_000),
        jobId: job.id,
        permittedPaths: source.permittedPaths,
        purpose,
      },
      ctx.workspace,
      ctx.signal,
    );
    return this.#create(
      job.id,
      "porter_patch",
      {
        id: sourceId,
        revision: source.revision,
        fingerprint: source.fingerprint,
      },
      {},
      preview,
    );
  }
  // These four methods are HOST CONTROL capabilities, never registered tools.
  async registerPorterSource(
    jobId,
    { sourceId = "primary", files, permittedPaths },
  ) {
    return this.#exclusive(jobId, async (job, workspace) => {
      check(
        job.profile === "porter",
        "WRONG_WORKFLOW",
        "Porter source requires a Porter job",
      );
      sourceKey(sourceId);
      files = bounded(files, 32_000);
      permittedPaths = bounded(permittedPaths, 4000);
      const snapshot = await this.#bridge(
        "porter.import_snapshot",
        { files, permittedPaths },
        workspace,
      );
      let previous;
      try {
        previous = await this.#source(jobId, sourceId);
      } catch (error) {
        if (error.code !== "ENOENT") throw error;
      }
      const source = {
        id: sourceId,
        jobId,
        revision: (previous?.revision ?? 0) + 1,
        fingerprint: snapshot.fingerprint,
        files,
        permittedPaths,
      };
      await (
        await this.#control(jobId)
      ).write(`sources/${sourceId}.json`, JSON.stringify(source));
      await this.#store.save(job);
      return {
        sourceId,
        revision: source.revision,
        fingerprint: source.fingerprint,
      };
    });
  }
  async getReview(jobId, reviewId) {
    return this.#public(await this.#load(jobId, reviewId));
  }
  async requestMakerRevision(jobId, { sourceOperationId, newSpec }) {
    return this.#exclusive(jobId, async (job, workspace) => {
      const result = await this.previewMakerFromTool(
        { jobId, workspace },
        { sourceOperationId, newSpec },
      );
      await this.#store.save(job);
      return result;
    });
  }
  async revokeReview(jobId, { reviewId }) {
    return this.#exclusive(jobId, async (job) => {
      const record = await this.#load(jobId, reviewId);
      if (["pending", "approved"].includes(record.status)) {
        record.status =
          record.status === "approved" &&
          record.approval?.expiresAt <= this.#now()
            ? "expired"
            : "cancelled";
        record.approval = null;
        await this.#save(record);
        await this.#store.save(job);
        await this.#store.event(jobId, "review.revoked", { reviewId });
      }
      return this.#public(record);
    });
  }
  async approveReview(jobId, { reviewId, expectedDigest }) {
    return this.#exclusive(jobId, async (job) => {
      const record = await this.#load(jobId, reviewId);
      check(
        record.status === "pending",
        "REVIEW_NOT_PENDING",
        "Review is no longer awaiting approval",
      );
      check(
        exactHex(expectedDigest) && expectedDigest === record.digest,
        "STALE_APPROVAL",
        "Approve the exact digest of the authoritative review shown by getReview",
      );
      const token = randomBytes(32).toString("hex");
      record.status = "approved";
      record.approval = {
        digest: record.digest,
        tokenHash: hashToken(token),
        expiresAt: this.#now() + this.#ttl,
      };
      await this.#save(record);
      await this.#store.save(job);
      await this.#store.event(jobId, "review.approved", { reviewId });
      return {
        reviewId,
        approvalToken: token,
        expiresAt: record.approval.expiresAt,
      };
    });
  }
  async applyApprovedReview(jobId, { reviewId, approvalToken }) {
    return this.#exclusive(jobId, async (job, workspace) => {
      const record = await this.#load(jobId, reviewId);
      check(
        record.status === "approved" && record.approval,
        "APPROVAL_REQUIRED",
        "A host-approved review is required",
      );
      check(
        exactHex(approvalToken) &&
          timingSafeEqual(
            Buffer.from(hashToken(approvalToken), "hex"),
            Buffer.from(record.approval.tokenHash, "hex"),
          ),
        "APPROVAL_REQUIRED",
        "Invalid host approval capability",
      );
      check(
        record.approval.digest === record.digest &&
          this.#now() < record.approval.expiresAt,
        "APPROVAL_EXPIRED",
        "Approval expired or no longer matches this review",
      );
      let bundle;
      try {
        if (
          record.kind === "maker_revision" ||
          record.kind === "maker_source_edit"
        ) {
          check(
            job.profile === "maker",
            "WRONG_WORKFLOW",
            "Maker review requires a Maker job",
          );
          const sourceRoot = await this.#makerSource(
            job,
            record.source.operationId,
          );
          const sourceEdit = record.kind === "maker_source_edit";
          if (sourceEdit)
            await captureSourceArtifact(
              this.#store,
              job,
              record.source.operationId,
            );
          const fresh = await this.#bridge(
            sourceEdit ? "maker.prepare_source_edit" : "maker.prepare_revision",
            {
              sourceRoot,
              ...(sourceEdit
                ? { request: record.payload.request }
                : { newSpec: record.payload.newSpec }),
              expectedRevision: record.source.revision,
            },
            workspace,
          );
          check(
            fresh.preview.revision === record.preview.revision,
            "STALE_REVIEW",
            "Maker source changed since review",
          );
          bundle = fresh.files;
        } else {
          check(
            job.profile === "porter",
            "WRONG_WORKFLOW",
            "Porter review requires a Porter job",
          );
          const source = await this.#source(jobId, record.source.id);
          check(
            source.revision === record.source.revision &&
              source.fingerprint === record.source.fingerprint,
            "STALE_REVIEW",
            "Porter source or host path scope changed since review",
          );
          const applied = await this.#bridge(
            "porter.apply_copy",
            {
              files: source.files,
              contract: record.preview,
              jobId,
              approvedDigest: record.preview.review_digest,
              permittedPaths: source.permittedPaths,
            },
            workspace,
          );
          bundle = Object.entries(applied.files).map(([relative, content]) => ({
            path: relative,
            base64: Buffer.from(content, "utf8").toString("base64"),
          }));
        }
      } catch (error) {
        record.status = "invalidated";
        record.approval = null;
        await this.#save(record);
        throw new RuntimeError(
          "STALE_REVIEW",
          `Reviewed inputs are no longer valid: ${String(error.message).slice(0, 300)}`,
        );
      }
      check(
        Array.isArray(bundle) && bundle.length <= 300,
        "REVIEW_SIZE_LIMIT",
        "Invalid review file bundle",
      );
      const outputDirectory = `reviewed/${reviewId}`;
      const names = new Set();
      const artifacts = bundle.map((file) => {
        check(
          typeof file.path === "string" &&
            !file.path.startsWith("/") &&
            !/[\\\x00-\x1f]/.test(file.path) &&
            file.path.split("/").every((p) => p && p !== "." && p !== "..") &&
            !names.has(file.path),
          "INVALID_REVIEW_PATH",
          "Invalid or duplicate reviewed file path",
        );
        names.add(file.path);
        const bytes = Buffer.from(file.base64, "base64");
        return {
          path: `${outputDirectory}/${file.path}`,
          bytes: bytes.length,
          sha256: createHash("sha256").update(bytes).digest("hex"),
        };
      });
      const result = {
        status: "reviewed_copy_created",
        outputDirectory,
        reviewId,
        reviewDigest: record.digest,
        artifacts,
        originalSourceModified: false,
        buildValidated: false,
        verificationState: "not_run",
      };
      const receipt = {
        id: `host-${reviewId}`,
        name:
          record.kind === "maker_revision"
            ? "maker.revise"
            : record.kind === "maker_source_edit"
              ? "maker.edit_source"
              : "porter.apply_patch",
        fingerprint: record.digest,
        status: "completed",
        result,
      };
      bounded([...job.operations, receipt], 128_000);
      record.status = "applying";
      record.approval = null;
      await this.#save(record);
      try {
        const directory = await workspace.resolve(outputDirectory, {
          parents: true,
        });
        await fs.mkdir(directory, { mode: 0o700 }); // Must be a fresh output, never merge/overwrite.
        for (let i = 0; i < bundle.length; i++) {
          await workspace.writeBytes(
            artifacts[i].path,
            Buffer.from(bundle[i].base64, "base64"),
          );
          const actual = await fs.readFile(
            await workspace.resolve(artifacts[i].path),
          );
          check(
            createHash("sha256").update(actual).digest("hex") ===
              artifacts[i].sha256,
            "WRITE_VERIFICATION_FAILED",
            "Reviewed-copy hash mismatch",
          );
        }
        job.operations.push(receipt);
        await this.#store.save(job);
        record.status = "applied";
        record.result = result;
        await this.#save(record);
        await this.#store.event(jobId, "review.applied", {
          reviewId,
          kind: record.kind,
        });
        return result;
      } catch (error) {
        record.status = "failed";
        record.errorCode = error.code ?? "REVIEW_APPLY_FAILED";
        await this.#save(record);
        throw error;
      }
    });
  }
}

export function registerReviewPreviewTools(registry, reviews) {
  const strictObject = (args, keys) => {
    check(
      args &&
        typeof args === "object" &&
        !Array.isArray(args) &&
        Object.keys(args).length === keys.length &&
        keys.every((key) => Object.hasOwn(args, key)),
      "INVALID_ARGUMENTS",
      "Expected exact review proposal fields; approval is a host capability",
    );
    bounded(args);
  };
  registry.register({
    name: "maker.preview_revision",
    description:
      "Propose a revision to a completed generation in this job. Returns a review digest; cannot approve or apply.",
    replaySafety: "manual",
    inputSchema: {
      type: "object",
      properties: {
        sourceOperationId: { type: "string" },
        newSpec: { type: "object" },
      },
      required: ["sourceOperationId", "newSpec"],
      additionalProperties: false,
    },
    validate: (args) => strictObject(args, ["sourceOperationId", "newSpec"]),
    execute: (args, ctx) => reviews.previewMakerFromTool(ctx, args),
  });
  registry.register({
    name: "maker.preview_source_edit",
    description:
      "Propose a locked source edit to this job artifact. Returns review only; cannot approve, apply or build.",
    replaySafety: "manual",
    inputSchema: {
      type: "object",
      properties: {
        sourceOperationId: { type: "string" },
        request: { type: "object" },
      },
      required: ["sourceOperationId", "request"],
      additionalProperties: false,
    },
    validate: (args) => {
      strictObject(args, ["sourceOperationId", "request"]);
      strictObject(args.request, ["schema_version", "edits", "lock_paths"]);
    },
    execute: (args, ctx) => reviews.previewMakerEditFromTool(ctx, args),
  });
  registry.register({
    name: "porter.preview_patch",
    description:
      "Propose text changes to a host-imported snapshot, within host-granted paths. Cannot approve or apply.",
    replaySafety: "manual",
    inputSchema: {
      type: "object",
      properties: {
        sourceId: { type: "string" },
        replacements: {
          type: "object",
          additionalProperties: { type: "string" },
        },
        purpose: { type: "string" },
      },
      required: ["sourceId", "replacements", "purpose"],
      additionalProperties: false,
    },
    validate: (args) =>
      strictObject(args, ["sourceId", "replacements", "purpose"]),
    execute: (args, ctx) => reviews.previewPorterFromTool(ctx, args),
  });
  return registry;
}
