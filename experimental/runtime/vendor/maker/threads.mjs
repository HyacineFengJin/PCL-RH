/** Thread metadata references existing Pi jobs; it is not a scheduler. The host
 * serializes admission and writes. A lost start reply is recovered from this
 * record. Applying a review and recording its immutable source are separate
 * effects: retries reconcile the source receipt and never replay an applied
 * review. Existing project notes and checkpoints retain their own CAS rules. */
import fs from "node:fs/promises";
import path from "node:path";
import { randomUUID } from "node:crypto";
import { Workspace } from "../../src/workspace.mjs";
const uuid = /^[a-f0-9]{8}-(?:[a-f0-9]{4}-){3}[a-f0-9]{12}(?![\s\S])/;
const modes = ["ask", "plan", "execute"];
function need(ok, message) {
  if (!ok) throw new Error(message);
}
function text(value, max, multiline = true, empty = false) {
  need(
    typeof value === "string" &&
      (empty || value.trim()) &&
      value.length <= max &&
      !/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f\p{Cf}\p{Cs}]/u.test(
        value,
      ),
    "Invalid thread text",
  );
  need(multiline || !/[\t\r\n]/.test(value), "Invalid single-line thread text");
  return value;
}
function validateScopes(scopes) {
  need(Array.isArray(scopes) && scopes.length <= 16, "Scope limit is 16");
  for (const scope of scopes) {
    need(
      scope &&
        (uuid.test(scope.id) ||
          /^domain:(items|blocks|entities|structures|rules|systems|assets|other)$/.test(
            scope.id,
          )) &&
        Array.isArray(scope.files) &&
        scope.files.length <= 32 &&
        [
          "items",
          "blocks",
          "entities",
          "structures",
          "rules",
          "systems",
          "assets",
          "other",
        ].includes(scope.kind),
      "Invalid scope hint",
    );
    text(scope.name, 120, false);
    text(scope.notes, 6000, true, true);
    for (const file of scope.files) {
      text(file, 512, false);
      need(
        !file.startsWith("/") &&
          !file.includes("\\") &&
          file
            .split("/")
            .every((part) => part && part !== "." && part !== ".."),
        "Invalid scope path",
      );
    }
  }
}
export class MakerThreads {
  constructor(root, { projects, job, create, start, abort, review, apply }) {
    this.files = new Workspace(path.join(root, "maker-threads"));
    Object.assign(this, { projects, job, create, start, abort, review, apply });
  }
  async init() {
    await this.files.init();
    return this;
  }
  async read(id) {
    need(uuid.test(id), "Invalid thread ID");
    return this.validate(
      JSON.parse((await this.files.read(`${id}.json`)).content),
      id,
    );
  }
  validate(value, id = value?.id) {
    const keys = [
      "schemaVersion",
      "id",
      "projectId",
      "name",
      "notes",
      "title",
      "createdAt",
      "updatedAt",
      "rounds",
    ];
    need(
      value && Object.keys(value).sort().join() === keys.sort().join(),
      "Unsupported thread record",
    );
    need(
      value.schemaVersion === 1 &&
        uuid.test(value.id) &&
        value.id === id &&
        (value.projectId === null || uuid.test(value.projectId)) &&
        Array.isArray(value.rounds) &&
        value.rounds.length > 0 &&
        value.rounds.length <= 64,
      "Unsupported thread record",
    );
    text(value.title, 120, false);
    text(value.name, 120, false);
    text(value.notes, 8192, true, true);
    need(
      [value.createdAt, value.updatedAt].every(
        (date) =>
          typeof date === "string" && new Date(date).toISOString() === date,
      ),
      "Invalid thread timestamp",
    );
    const jobs = new Set();
    for (const round of value.rounds) {
      need(
        round &&
          Object.keys(round).every((key) =>
            ["jobId", "request", "mode", "scopes", "checkpointId"].includes(
              key,
            ),
          ) &&
          uuid.test(round.jobId) &&
          !jobs.has(round.jobId) &&
          modes.includes(round.mode) &&
          (round.checkpointId === null || uuid.test(round.checkpointId)),
        "Invalid thread round",
      );
      jobs.add(round.jobId);
      text(round.request, 12000);
      validateScopes(round.scopes ?? []);
    }
    return value;
  }
  async save(record) {
    const data = JSON.stringify(this.validate(record));
    need(Buffer.byteLength(data) <= 800000, "Thread history exceeds 800 KB");
    await this.files.write(`${record.id}.json`, data);
  }
  async list() {
    const names = (await fs.readdir(this.files.root)).filter((name) =>
      name.endsWith(".json"),
    );
    need(names.length <= 128, "Thread limit is 128");
    const threads = [],
      errors = [];
    for (const name of names) {
      try {
        threads.push(await this.read(name.slice(0, -5)));
      } catch {
        errors.push(name.slice(0, -5));
      } // Retain corrupt records; never overwrite as empty.
    }
    return {
      threads: threads.sort((a, b) => b.updatedAt.localeCompare(a.updatedAt)),
      errors,
    };
  }
  async run(args) {
    const mode = args.mode ?? "execute",
      prompt = text(args.prompt, 12000),
      request = text(args.request ?? args.prompt, 12000);
    need(modes.includes(mode), "Choose ask, plan or execute");
    const scopes = args.scopes ?? [];
    validateScopes(scopes);
    let record = args.threadId ? await this.read(args.threadId) : null;
    const project = args.id ? await this.projects.get(args.id) : null;
    if (project) {
      need(
        project.revision === args.expectedRevision,
        "Project changed; refresh before continuing",
      );
      need(!project.archived, "Unarchive the project before continuing");
      need(
        project.checkpoints.some((point) => point.id === args.checkpointId),
        "Choose a source version",
      );
    }
    if (record)
      need(
        record.projectId === (project?.id ?? null),
        "Thread belongs to another project",
      );
    else {
      const existing = await this.list();
      need(
        existing.threads.length + existing.errors.length < 128,
        "Thread limit is 128",
      );
      const now = new Date().toISOString();
      record = {
        schemaVersion: 1,
        id: randomUUID(),
        projectId: project?.id ?? null,
        name: project?.name ?? text(args.name, 120, false).trim(),
        notes: project?.notes ?? args.notes ?? "",
        title: request.trim().split("\n")[0].slice(0, 120),
        createdAt: now,
        updatedAt: now,
        rounds: [],
      };
    }
    text(record.notes, 8192, true, true);
    need(record.rounds.length < 64, "Thread round limit is 64");
    // Read-only plans can be followed by execution, but an unreviewed execution
    // must be dealt with before another round can silently change its base.
    if (record.rounds.length) {
      const previous = await this.job(record.rounds.at(-1).jobId);
      need(
        !["queued", "running"].includes(previous.status),
        "Wait for this thread to finish",
      );
      for (const id of previous.reviewIds ?? []) {
        const review = await this.review(previous.id, id);
        need(
          !["pending", "approved"].includes(review.status),
          "Review or return the previous change set first",
        );
      }
    }
    const recent = [];
    for (const round of record.rounds.slice(-4)) {
      const previous = await this.job(round.jobId);
      recent.push({
        request: round.request.slice(0, 2000),
        mode: round.mode,
        response:
          typeof previous.result?.text === "string"
            ? previous.result.text.slice(0, 2000)
            : "",
      });
    }
    const created = await this.create({
      ...args,
      prompt,
      makerThread: {
        id: record.id,
        mode,
        recent,
        charter: project?.notes ?? record.notes,
      },
    });
    record.rounds.push({
      jobId: created.jobId,
      request,
      mode,
      scopes,
      checkpointId: args.checkpointId ?? null,
    });
    record.updatedAt = new Date().toISOString();
    try {
      await this.save(record);
    } catch (error) {
      await this.abort?.(created.jobId);
      throw error;
    }
    // The shared runtime starts only after durable thread admission. A failed
    // start leaves a discoverable interrupted/cancelled round, never a ghost.
    await this.start?.(created.jobId);
    return record;
  }
  async archive({ threadId, jobId, operationId }) {
    const record = await this.read(threadId);
    need(
      record.rounds.some((round) => round.jobId === jobId),
      "Source does not belong to this thread",
    );
    const job = await this.job(jobId);
    need(
      job.profile === "maker" && job.input.makerThread?.id === record.id,
      "Job belongs to another thread",
    );
    need(
      !["queued", "running"].includes(job.status),
      "Wait for the thread to finish",
    );
    const all = await this.projects.list();
    // Initial registration may have succeeded before its thread link was saved.
    const linked = record.projectId
      ? all.find((project) => project.id === record.projectId)
      : all.find((project) =>
          project.checkpoints.some(
            (point) =>
              point.jobId === jobId && point.operationId === operationId,
          ),
        );
    let project = linked;
    if (project) {
      need(
        !project.archived,
        "Unarchive the project before recording a version",
      );
      if (
        !project.checkpoints.some(
          (point) => point.jobId === jobId && point.operationId === operationId,
        )
      )
        project = await this.projects.update({
          id: project.id,
          expectedRevision: project.revision,
          name: project.name,
          notes: project.notes,
          archived: false,
          source: { jobId, operationId, label: record.title },
        });
    } else {
      need(record.projectId === null, "Project record is unavailable");
      project = await this.projects.create({
        name: record.name,
        notes: record.notes,
        jobId,
        operationId,
        label: record.title,
      });
    }
    record.projectId = project.id;
    record.updatedAt = new Date().toISOString();
    await this.save(record);
    return { project, thread: record };
  }
  async approve({ threadId, jobId, reviewId, digest }) {
    const record = await this.read(threadId);
    need(
      record.rounds.some(
        (round) => round.jobId === jobId && round.mode === "execute",
      ),
      "Review does not belong to an execution round",
    );
    const job = await this.job(jobId);
    need(
      job.profile === "maker" && job.input.makerThread?.id === record.id,
      "Job belongs to another thread",
    );
    need(
      !["queued", "running"].includes(job.status),
      "Wait for the thread to finish",
    );
    if (record.projectId) {
      const project = await this.projects.get(record.projectId);
      need(!project.archived, "Unarchive the project before approval");
      need(
        project.checkpoints.length < 128 ||
          project.checkpoints.some(
            (point) =>
              point.jobId === jobId && point.operationId === `host-${reviewId}`,
          ),
        "Project version limit is 128",
      );
    }
    const review = await this.review(jobId, reviewId);
    need(
      review.reviewDigest === digest &&
        (["pending", "applied"].includes(review.status) ||
          job.operations.some(
            (op) =>
              op.id === `host-${reviewId}` &&
              op.status === "completed" &&
              op.result?.outputDirectory,
          )),
      "Review changed; reload before approval",
    );
    if (review.status === "pending") await this.apply(jobId, reviewId, digest);
    return this.archive({ threadId, jobId, operationId: `host-${reviewId}` });
  }
}
/** Host-owned plan receipt. Steps describe intent, not proof of execution. */
export const makerPlanTool = {
  name: "maker.report_plan",
  description:
    "Record a concise plan before proposing source changes. These are intentions, not completed work.",
  replaySafety: "read-only",
  inputSchema: {
    type: "object",
    properties: {
      steps: {
        type: "array",
        minItems: 1,
        maxItems: 12,
        items: { type: "string", maxLength: 240 },
      },
    },
    required: ["steps"],
    additionalProperties: false,
  },
  validate(args) {
    need(
      args &&
        Object.keys(args).join() === "steps" &&
        Array.isArray(args.steps) &&
        args.steps.length > 0 &&
        args.steps.length <= 12,
      "Provide 1–12 plan steps",
    );
    args.steps.forEach((step) => text(step, 240));
  },
  execute: async (args) => ({ steps: args.steps }),
};
