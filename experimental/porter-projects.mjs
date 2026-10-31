/** Host-owned persistent Porter projects. Runtime jobs remain single-round
 * immutable runs; project discussion and source baselines outlive those runs.
 * Per-project serialization avoids lost user messages during AI tool callbacks.
 * Malformed/future/external-edited records are retained and writes are refused.
 * No raw credential/provider configuration is accepted by this store. */
import fs from "node:fs/promises";
import { constants } from "node:fs";
import path from "node:path";
import { randomUUID, createHash } from "node:crypto";
import { Workspace } from "./runtime/src/workspace.mjs";
import { check, cloneJson } from "./runtime/src/errors.mjs";
import { parsePorterOrigin } from "./porter-origins.mjs";

const UUID = /^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/;
const TERMINAL = new Set(["completed", "failed", "cancelled", "interrupted"]);
const validDate = (value) =>
  typeof value === "string" &&
  value.length <= 50 &&
  Number.isFinite(Date.parse(value));
const boundedText = (value, limit) =>
  typeof value === "string" && value.trim().length > 0 && value.length <= limit;
const hash = (text) => createHash("sha256").update(text).digest("hex");
export const porterSnapshotFingerprint = (files) =>
  hash(
    JSON.stringify(
      Object.fromEntries(
        Object.entries(files).sort(([a], [b]) => a.localeCompare(b)),
      ),
    ),
  );
const text = (value, max, label) => {
  check(
    typeof value === "string" && value.trim() && value.length <= max,
    "INVALID_PROJECT",
    `${label} must be nonempty text up to ${max} characters`,
  );
  return value.trim();
};
function sourceReference(value) {
  try {
    const url = new URL(value);
    return url.protocol === "https:" &&
      !url.username &&
      !url.password &&
      url.href.length <= 1000
      ? url.href
      : null;
  } catch {
    return null;
  }
}
function snapshot(input) {
  check(
    input &&
      typeof input.files === "object" &&
      input.files &&
      !Array.isArray(input.files) &&
      Object.keys(input.files).length > 0 &&
      Object.keys(input.files).length <= 300 &&
      Object.entries(input.files).every(
        ([p, v]) =>
          typeof v === "string" &&
          p.length <= 500 &&
          !/[\\:\x00-\x1f]/.test(p) &&
          p.split("/").every((x) => x && x !== "." && x !== ".."),
      ),
    "INVALID_SOURCE",
    "Expected an imported text snapshot with canonical paths",
  );
  const files = cloneJson(input.files, {
    maxBytes: 30000,
    code: "SOURCE_SIZE_LIMIT",
  });
  check(
    Array.isArray(input.permittedPaths) &&
      new Set(input.permittedPaths).size === input.permittedPaths.length &&
      input.permittedPaths.every((p) => Object.hasOwn(files, p)),
    "INVALID_GRANTS",
    "Grants must select existing imported files",
  );
  check(
    (input.revision === undefined ||
      (Number.isSafeInteger(input.revision) && input.revision >= 1)) &&
      (input.label === undefined || boundedText(input.label, 200)),
    "INVALID_SOURCE",
    "Source revision or label is invalid",
  );
  return {
    files,
    permittedPaths: [...input.permittedPaths],
    fingerprint: porterSnapshotFingerprint(files),
    label: input.label || "imported-source",
    revision: input.revision || 1,
  };
}

export class PorterProjects {
  #root;
  #runtime;
  #queues = new Map();
  // Remember exact UTF-8 generations, not every project's full source/history.
  // The current read and write CAS still compare the actual raw bytes below.
  #seen = new Map();
  constructor(root, runtime) {
    this.#root = path.join(root, "porter-projects");
    this.#runtime = runtime;
  }
  async init() {
    await new Workspace(this.#root).init();
    return this;
  }
  #directory(id) {
    check(UUID.test(id), "INVALID_PROJECT_ID", "Invalid Porter project ID");
    return path.join(this.#root, id);
  }
  async #read(id) {
    const directory = this.#directory(id);
    const stat = await fs.lstat(directory);
    check(
      stat.isDirectory() &&
        !stat.isSymbolicLink() &&
        stat.uid === process.getuid() &&
        !(stat.mode & 0o077),
      "INVALID_PROJECT",
      "Expected a private project directory",
    );
    const handle = await fs.open(
      path.join(directory, "project.json"),
      constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK,
    );
    let raw;
    try {
      const s = await handle.stat();
      check(
        s.isFile() &&
          s.nlink <= 1 &&
          s.uid === process.getuid() &&
          !(s.mode & 0o077) &&
          s.size <= 800000,
        "INVALID_PROJECT",
        "Project file is unsafe or too large; retained for recovery",
      );
      raw = await handle.readFile("utf8");
    } finally {
      await handle.close();
    }
    const generation = hash(raw);
    check(
      !this.#seen.has(id) || this.#seen.get(id) === generation,
      "PROJECT_CHANGED",
      "Project changed externally; original file retained. Repair it and restart the host before writing.",
    );
    const p = JSON.parse(raw);
    check(
      Buffer.byteLength(raw) <= 800000 &&
        p.schemaVersion === 1 &&
        p.id === id &&
        Number.isSafeInteger(p.revision) &&
        p.revision >= 1 &&
        boundedText(p.name, 160) &&
        boundedText(p.goal, 2000) &&
        boundedText(p.targetId, 100) &&
        validDate(p.createdAt) &&
        validDate(p.updatedAt) &&
        (p.identifierProfile === null ||
          p.identifierProfile === "fabric-yarn-1.20.6-to-1.21-identifier-v1") &&
        Array.isArray(p.messages) &&
        p.messages.length <= 256 &&
        Array.isArray(p.rounds) &&
        p.rounds.length <= 64 &&
        ["unknown", "owner", "permission", "license-reviewed"].includes(
          p.rights,
        ) &&
        typeof p.archived === "boolean" &&
        typeof p.acknowledgeBeta === "boolean" &&
        p.messages.every(
          (m) =>
            UUID.test(m.id) &&
            ["user", "assistant", "system"].includes(m.role) &&
            [
              "request",
              "bug",
              "feedback",
              "answer",
              "baseline",
              "question",
              "progress",
              "round-result",
            ].includes(m.kind) &&
            boundedText(m.content, 18000) &&
            validDate(m.createdAt) &&
            (m.resolved === undefined || typeof m.resolved === "boolean") &&
            (m.options === undefined ||
              (Array.isArray(m.options) &&
                m.options.length <= 6 &&
                m.options.every(
                  (v) => typeof v === "string" && v.length <= 200,
                ))) &&
            (m.details === undefined ||
              ["completed", "remaining", "limitations"].every(
                (k) =>
                  Array.isArray(m.details[k]) &&
                  m.details[k].length <= 16 &&
                  m.details[k].every(
                    (v) => typeof v === "string" && v.length <= 500,
                  ),
              )),
        ) &&
        p.rounds.every(
          (r) =>
            UUID.test(r.id) &&
            UUID.test(r.jobId) &&
            ["template", "live"].includes(r.mode) &&
            typeof r.responseRecorded === "boolean" &&
            Number.isSafeInteger(r.baselineRevision) &&
            r.baselineRevision >= 1 &&
            (r.sourceFingerprint === undefined ||
              (typeof r.sourceFingerprint === "string" &&
                /^[a-f0-9]{64}$/.test(r.sourceFingerprint))) &&
            (r.sourceRegistered === undefined ||
              typeof r.sourceRegistered === "boolean") &&
            boundedText(r.targetId, 100) &&
            validDate(r.createdAt) &&
            [
              "running",
              "completed",
              "failed",
              "cancelled",
              "interrupted",
            ].includes(r.status),
        ) &&
        new Set(p.messages.map((m) => m.id)).size === p.messages.length &&
        new Set(p.rounds.map((r) => r.id)).size === p.rounds.length &&
        (p.activeRound === null ||
          p.rounds.some(
            (r) => r.id === p.activeRound && !r.responseRecorded,
          )) &&
        (p.awaitingQuestion === null ||
          p.messages.some(
            (m) => m.id === p.awaitingQuestion && m.kind === "question",
          )),
      "INVALID_PROJECT",
      "Project record is invalid or unsupported; retained for recovery",
    );
    if (p.snapshot)
      check(
        snapshot(p.snapshot).fingerprint === p.snapshot.fingerprint,
        "INVALID_SOURCE",
        "Source baseline fingerprint changed; original file retained",
      );
    this.#seen.set(id, generation);
    return { project: p, raw };
  }
  async #write(p, previous) {
    const directory = this.#directory(p.id);
    if (previous !== null)
      check(
        (await this.#read(p.id)).raw === previous,
        "PROJECT_CHANGED",
        "Project changed externally; reload before writing",
      );
    p.revision++;
    p.updatedAt = new Date().toISOString();
    const raw = JSON.stringify(p);
    // Active rounds reserve room for their final outcome, even if users keep
    // adding feedback. Reaching a limit must not strand project admission.
    check(
      Buffer.byteLength(raw) <= (p.activeRound ? 750000 : 800000) &&
        p.messages.length <= 256 &&
        p.rounds.length <= 64,
      "PROJECT_SIZE_LIMIT",
      "Project history limit reached; original record retained",
    );
    await new Workspace(directory)
      .init()
      .then((w) => w.write("project.json", raw));
    this.#seen.set(p.id, hash(raw));
  }
  async #exclusive(id, mutate) {
    const previous = this.#queues.get(id) || Promise.resolve();
    const next = previous
      .catch(() => {})
      .then(async () => {
        const { project, raw } = await this.#read(id);
        const result = await mutate(project);
        await this.#write(project, raw);
        return result ?? cloneJson(project);
      });
    this.#queues.set(id, next);
    try {
      return await next;
    } finally {
      if (this.#queues.get(id) === next) this.#queues.delete(id);
    }
  }
  async create({
    name,
    goal,
    targetId,
    rights,
    acknowledgeBeta,
    identifierProfile,
    origin,
    source,
  }) {
    const entries = await fs.readdir(this.#root);
    check(
      entries.filter((id) => UUID.test(id)).length < 128,
      "PROJECT_LIMIT",
      "At most 128 Porter projects are supported",
    );
    const id = randomUUID(),
      now = new Date().toISOString();
    const p = {
      schemaVersion: 1,
      id,
      revision: 0,
      name: text(name, 160, "Project name"),
      goal: text(goal, 2000, "Migration goal"),
      targetId: text(targetId, 100, "Target"),
      rights,
      acknowledgeBeta: acknowledgeBeta === true,
      identifierProfile: identifierProfile ?? null,
      origin: origin
        ? {
            ...parsePorterOrigin(origin.url),
            ...(typeof origin.title === "string"
              ? { title: origin.title.slice(0, 160) }
              : {}),
            ...(sourceReference(origin.sourceUrl)
              ? { sourceUrl: sourceReference(origin.sourceUrl) }
              : {}),
            ...(typeof origin.license === "string"
              ? { license: origin.license.slice(0, 100) }
              : {}),
          }
        : null,
      snapshot: source ? snapshot(source) : null,
      archived: false,
      messages: [],
      rounds: [],
      activeRound: null,
      awaitingQuestion: null,
      createdAt: now,
      updatedAt: now,
    };
    check(
      ["unknown", "owner", "permission", "license-reviewed"].includes(rights),
      "INVALID_RIGHTS",
      "Choose source modification rights",
    );
    check(
      p.identifierProfile === null ||
        p.identifierProfile === "fabric-yarn-1.20.6-to-1.21-identifier-v1",
      "INVALID_PROFILE",
      "Unsupported source profile",
    );
    await fs.mkdir(this.#directory(id), { mode: 0o700 });
    await this.#write(p, null);
    return p;
  }
  async list() {
    const items = [],
      warnings = [];
    for (const id of (await fs.readdir(this.#root))
      .filter((id) => UUID.test(id))
      .slice(0, 128)) {
      try {
        const p = await this.read(id);
        items.push({
          id,
          revision: p.revision,
          name: p.name,
          targetId: p.targetId,
          updatedAt: p.updatedAt,
          archived: p.archived,
          hasSource: !!p.snapshot,
          awaitingQuestion: !!p.awaitingQuestion,
          activeRound: p.activeRound,
          latestJobId: p.rounds.at(-1)?.jobId || null,
          jobIds: p.rounds.map((r) => r.jobId),
          rounds: p.rounds.length,
        });
      } catch {
        warnings.push(id);
      }
    }
    return {
      projects: items.sort((a, b) => b.updatedAt.localeCompare(a.updatedAt)),
      unreadableProjectIds: warnings,
    };
  }
  async read(id) {
    await this.reconcile(id);
    return (await this.#read(id)).project;
  }
  // Comparison never reconciles or rewrites project history. The original
  // external-edit and baseline checks still arbitrate every retained read.
  async readRetained(id) {
    return (await this.#read(id)).project;
  }
  async addMessage(
    id,
    { revision, content, kind = "request", replyTo = null },
  ) {
    return this.#exclusive(id, async (p) => {
      check(
        p.revision === revision,
        "PROJECT_CHANGED",
        "Project changed; reload before sending",
      );
      check(
        !p.archived && ["request", "bug", "feedback", "answer"].includes(kind),
        "INVALID_MESSAGE",
        "Choose a supported message kind in an active project",
      );
      check(
        p.messages.length < (p.activeRound ? 254 : 256),
        "PROJECT_SIZE_LIMIT",
        "Discussion limit reached; space is reserved for the round result",
      );
      if (replyTo)
        check(
          p.awaitingQuestion === replyTo,
          "STALE_QUESTION",
          "This question is no longer awaiting an answer",
        );
      p.messages.push({
        id: randomUUID(),
        role: "user",
        kind,
        content: text(content, 8000, "Message"),
        replyTo,
        createdAt: new Date().toISOString(),
      });
      if (replyTo) p.awaitingQuestion = null;
    });
  }
  async setSource(id, { revision, source }) {
    return this.#exclusive(id, async (p) => {
      check(
        p.revision === revision && !p.activeRound && !p.archived,
        "PROJECT_CHANGED",
        "Reload and finish the current round before changing the baseline",
      );
      p.snapshot = snapshot({
        ...source,
        revision: (p.snapshot?.revision || 0) + 1,
      });
      p.messages.push({
        id: randomUUID(),
        role: "system",
        kind: "baseline",
        content: "Source baseline selected: " + p.snapshot.label,
        createdAt: new Date().toISOString(),
      });
    });
  }
  async configure(
    id,
    {
      revision,
      name,
      goal,
      targetId,
      rights,
      acknowledgeBeta,
      identifierProfile,
      source,
    },
  ) {
    return this.#exclusive(id, async (p) => {
      check(
        p.revision === revision && !p.activeRound && !p.archived,
        "PROJECT_CHANGED",
        "Reload and finish the current round before changing project configuration",
      );
      p.name = text(name, 160, "Project name");
      p.goal = text(goal, 2000, "Migration goal");
      p.targetId = text(targetId, 100, "Target");
      check(
        ["unknown", "owner", "permission", "license-reviewed"].includes(rights),
        "INVALID_RIGHTS",
        "Choose source modification rights",
      );
      check(
        identifierProfile == null ||
          identifierProfile === "fabric-yarn-1.20.6-to-1.21-identifier-v1",
        "INVALID_PROFILE",
        "Unsupported source profile",
      );
      p.rights = rights;
      p.acknowledgeBeta = acknowledgeBeta === true;
      p.identifierProfile = identifierProfile ?? null;
      if (source) {
        p.snapshot = snapshot({
          ...source,
          revision: (p.snapshot?.revision || 0) + 1,
        });
        p.messages.push({
          id: randomUUID(),
          role: "system",
          kind: "baseline",
          content: "Source baseline and file grants updated.",
          createdAt: new Date().toISOString(),
        });
      }
    });
  }
  async resolveMessage(id, { revision, messageId, resolved }) {
    return this.#exclusive(id, async (p) => {
      check(
        p.revision === revision && !p.archived && typeof resolved === "boolean",
        "PROJECT_CHANGED",
        "Reload before changing issue status",
      );
      const message = p.messages.find((m) => m.id === messageId);
      check(
        message?.role === "user" && ["request", "bug"].includes(message.kind),
        "INVALID_MESSAGE",
        "Only user requirements and BUG reports can be resolved",
      );
      message.resolved = resolved;
    });
  }
  async archive(id, { revision, archived }) {
    return this.#exclusive(id, async (p) => {
      check(
        p.revision === revision &&
          !p.activeRound &&
          typeof archived === "boolean",
        "PROJECT_CHANGED",
        "Reload and finish the current round before archiving",
      );
      p.archived = archived;
    });
  }
  async beginRound(id, { revision, mode }, createJob) {
    return this.#exclusive(id, async (p) => {
      check(
        p.revision === revision &&
          !p.activeRound &&
          !p.archived &&
          !p.awaitingQuestion,
        "PROJECT_CHANGED",
        "Reload, answer pending questions and finish the current round before continuing",
      );
      check(
        p.snapshot &&
          ["template", "live"].includes(mode) &&
          p.rounds.length < 64,
        "SOURCE_REQUIRED",
        "Import source before starting a round",
      );
      check(
        p.messages.length < 254,
        "PROJECT_SIZE_LIMIT",
        "Project discussion limit reached; retain this project and start a new one",
      );
      // Preserve all open user requirements/BUGs, then include recent messages
      // within the existing engine budget. Full discussion stays on disk; the
      // prompt explicitly describes its reduced context rather than truncating
      // text or pretending that omitted history was read by this round.
      const selected = p.messages.filter(
        (m) =>
          m.role === "user" &&
          ["request", "bug"].includes(m.kind) &&
          !m.resolved,
      );
      cloneJson(selected, { maxBytes: 16000, code: "OPEN_ISSUES_SIZE_LIMIT" });
      for (const message of [...p.messages].reverse()) {
        if (selected.includes(message)) continue;
        if (Buffer.byteLength(JSON.stringify([...selected, message])) <= 22000)
          selected.push(message);
      }
      const included = new Set(selected.map((m) => m.id));
      const conversation = p.messages
        .filter((m) => included.has(m.id))
        .map(({ role, kind, content, replyTo, details, resolved }) => ({
          role,
          kind,
          content,
          ...(replyTo ? { replyTo } : {}),
          ...(details ? { details } : {}),
          ...(resolved ? { resolved } : {}),
        }));
      const roundId = randomUUID();
      const input = {
        files: p.snapshot.files,
        targetId: p.targetId,
        rights: p.rights,
        acknowledgeBeta: p.acknowledgeBeta,
        ...(p.identifierProfile
          ? { identifierProfile: p.identifierProfile }
          : {}),
        porterProject: {
          id,
          roundId,
          baselineRevision: p.snapshot.revision,
          sourceFingerprint: p.snapshot.fingerprint,
          sourceRegistered: p.snapshot.permittedPaths.length > 0,
          conversationThrough: p.messages.at(-1)?.id || null,
        },
        goal: p.goal,
        conversation,
        discussionContext: {
          includedMessages: conversation.length,
          omittedMessages: p.messages.length - conversation.length,
          scope: "project-goal-open-user-issues-and-recent-discussion",
        },
        ...(mode === "live" ? { prompt: p.goal } : {}),
      };
      const job = await createJob(input, p.snapshot.permittedPaths, mode);
      p.rounds.push({
        id: roundId,
        jobId: job.id,
        mode,
        baselineRevision: p.snapshot.revision,
        sourceFingerprint: p.snapshot.fingerprint,
        sourceRegistered: p.snapshot.permittedPaths.length > 0,
        targetId: p.targetId,
        conversationThrough: input.porterProject.conversationThrough,
        createdAt: new Date().toISOString(),
        status: "running",
        responseRecorded: false,
      });
      p.activeRound = roundId;
      return { jobId: job.id, projectId: id };
    });
  }
  async askUser(jobId, { question, options = [] }) {
    const job = await this.#runtime.getJob(jobId),
      binding = job.input.porterProject;
    check(
      binding,
      "PROJECT_REQUIRED",
      "This tool requires a Porter project round",
    );
    let reply;
    await this.#exclusive(binding.id, async (p) => {
      check(
        p.activeRound === binding.roundId &&
          p.rounds.some((r) => r.id === binding.roundId && r.jobId === jobId) &&
          !p.awaitingQuestion,
        "STALE_ROUND",
        "The round is no longer available for questions",
      );
      check(
        p.messages.length < 255,
        "PROJECT_SIZE_LIMIT",
        "Question history limit reached",
      );
      check(
        Array.isArray(options) &&
          options.length <= 6 &&
          options.every((o) => typeof o === "string" && o.length <= 200),
        "INVALID_QUESTION",
        "Use at most six short options",
      );
      const message = {
        id: randomUUID(),
        role: "assistant",
        kind: "question",
        jobId,
        content: text(question, 2000, "Question"),
        options,
        createdAt: new Date().toISOString(),
      };
      p.messages.push(message);
      p.awaitingQuestion = message.id;
      reply = {
        status: "awaiting_user",
        questionId: message.id,
        text: message.content,
        options,
      };
    });
    return reply;
  }
  async reportProgress(jobId, { summary, completed, remaining, limitations }) {
    const job = await this.#runtime.getJob(jobId),
      binding = job.input.porterProject;
    check(
      binding,
      "PROJECT_REQUIRED",
      "This tool requires a Porter project round",
    );
    return this.#exclusive(binding.id, async (p) => {
      check(
        p.activeRound === binding.roundId &&
          p.rounds.some((r) => r.id === binding.roundId && r.jobId === jobId),
        "STALE_ROUND",
        "The round is no longer active",
      );
      check(
        p.messages.length < 254,
        "PROJECT_SIZE_LIMIT",
        "Progress history limit reached; space is reserved for the round result",
      );
      for (const list of [completed, remaining, limitations])
        check(
          Array.isArray(list) &&
            list.length <= 16 &&
            list.every(
              (item) => typeof item === "string" && item.length <= 500,
            ),
          "INVALID_PROGRESS",
          "Progress lists must be bounded text",
        );
      p.messages.push({
        id: randomUUID(),
        role: "assistant",
        kind: "progress",
        jobId,
        content: text(summary, 2000, "Summary"),
        details: { completed, remaining, limitations },
        createdAt: new Date().toISOString(),
      });
      return { status: "recorded", verification: "model-proposal-unverified" };
    });
  }
  async reconcile(id) {
    const { project: before } = await this.#read(id);
    const unfinished = before.rounds.filter((r) => !r.responseRecorded);
    if (!unfinished.length) return;
    const outcomes = [];
    for (const round of unfinished) {
      try {
        const job = await this.#runtime.getJob(round.jobId);
        const status =
          ["running", "queued"].includes(job.status) &&
          !this.#runtime.hasActiveJob(job.id)
            ? "interrupted"
            : job.status;
        if (TERMINAL.has(status))
          outcomes.push({ roundId: round.id, job, status });
      } catch {
        outcomes.push({
          roundId: round.id,
          job: {
            id: round.jobId,
            error: {
              message: "Round record unavailable; original project retained.",
            },
          },
          status: "interrupted",
        });
      }
    }
    if (!outcomes.length) return;
    await this.#exclusive(id, async (p) => {
      for (const { roundId, job, status } of outcomes) {
        const round = p.rounds.find((r) => r.id === roundId);
        if (!round || round.responseRecorded) continue;
        round.status = status;
        round.responseRecorded = true;
        if (p.activeRound === round.id) p.activeRound = null;
        if (job.result?.label === "PORTER_AWAITING_USER") continue; // Question was already durably recorded by the tool.
        const response =
          job.result?.text ||
          job.error?.message ||
          `Static analysis round ${status}. Review risks and unsupported work; no compilation or game run.`;
        p.messages.push({
          id: randomUUID(),
          role: job.result?.text ? "assistant" : "system",
          kind: "round-result",
          jobId: job.id,
          content: String(response).slice(0, 18000),
          createdAt: new Date().toISOString(),
          verification: "unverified",
        });
      }
    });
  }
}
