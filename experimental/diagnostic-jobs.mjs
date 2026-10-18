/** Native-only text analysis. Rust owns the report receipt and exact previewed
 * bytes; this service owns single-use provider snapshots and diagnostic jobs.
 * Neither report text nor model output can grant tools or change the route.
 * Confirmations are deliberately memory-only so a host restart requires review.
 */
import { randomUUID } from "node:crypto";
import { readJobSnapshot } from "./job-snapshot.mjs";
import { publicPiSelection } from "./runtime/src/providers/pi-selection.mjs";

export const MAX_DIAGNOSTIC_REPORT_BYTES = 48_000;
const CONFIRMATION_TTL_MS = 10 * 60 * 1000;
const MAX_CONFIRMATIONS = 8;
const REPORT_ID = /^[a-f0-9]{64}$/;
const TOKEN = /^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/;

function fields(value, allowed) {
  if (
    !value ||
    typeof value !== "object" ||
    Array.isArray(value) ||
    Object.keys(value).some((key) => !allowed.includes(key))
  )
    throw new Error("Invalid diagnostic request fields");
}
function ownerFields(args) {
  fields(args, ["reportId", "jobId"]);
  if (!REPORT_ID.test(args.reportId) || !TOKEN.test(args.jobId))
    throw new Error("Invalid diagnostic report or task identity");
}

export class DiagnosticJobs {
  #confirmations = new Map();
  constructor({
    runtime,
    presets,
    provider,
    listJobs,
    start,
    cancel,
    hasCurrentJob,
    now = Date.now,
  }) {
    Object.assign(this, {
      runtime,
      presets,
      provider,
      listJobs,
      start,
      cancel,
      hasCurrentJob,
      now,
    });
  }
  #expire() {
    const now = this.now();
    for (const [token, value] of this.#confirmations)
      if (value.expiresAt <= now) this.#confirmations.delete(token);
  }
  hasActiveWork() {
    this.#expire();
    return this.#confirmations.size !== 0;
  }
  clearConfirmations() {
    this.#confirmations.clear();
  }
  preview(args) {
    fields(args, ["reportId"]);
    if (!REPORT_ID.test(args.reportId))
      throw new Error("Invalid native diagnostic report identity");
    this.#expire();
    const preset = this.presets.selected();
    const warning = this.presets.view().warning;
    if (!preset)
      return {
        ready: false,
        confirmationToken: null,
        selectedPreset: null,
        warning,
        maxReportBytes: MAX_DIAGNOSTIC_REPORT_BYTES,
      };
    // Snapshot even when the same preset ID is later edited or deleted. The
    // token represents the destination the user saw, not a mutable preset ID.
    const captured = structuredClone({
      selection: preset.selection,
      limits: preset.limits,
    });
    const token = randomUUID();
    if (this.#confirmations.size >= MAX_CONFIRMATIONS)
      this.#confirmations.delete(this.#confirmations.keys().next().value);
    this.#confirmations.set(token, {
      ...captured,
      reportId: args.reportId,
      expiresAt: this.now() + CONFIRMATION_TTL_MS,
    });
    return {
      ready: true,
      confirmationToken: token,
      selectedPreset: structuredClone({
        id: preset.id,
        name: preset.name,
        selection: publicPiSelection(captured.selection),
        limits: captured.limits,
      }),
      warning,
      maxReportBytes: MAX_DIAGNOSTIC_REPORT_BYTES,
    };
  }
  async analyze(args) {
    fields(args, ["reportId", "reportText", "confirmationToken"]);
    if (
      !REPORT_ID.test(args.reportId) ||
      typeof args.reportText !== "string" ||
      !args.reportText.trim() ||
      Buffer.byteLength(args.reportText) > MAX_DIAGNOSTIC_REPORT_BYTES
    )
      throw new Error(
        "Provide a native diagnostic report of 1–48000 UTF-8 bytes",
      );
    const input = {
      diagnostic: { schemaVersion: 1, reportId: args.reportId },
      reportText: args.reportText,
    };
    // Preserve the runtime's existing input limit, including JSON escapes.
    if (Buffer.byteLength(JSON.stringify(input)) > 64_000)
      throw new Error("Diagnostic report exceeds the serialized input limit");
    this.#expire();
    if (
      !TOKEN.test(args.confirmationToken) ||
      !this.#confirmations.has(args.confirmationToken)
    )
      throw new Error(
        "Diagnostic AI confirmation expired; preview the report and destination again",
      );
    if (
      (await this.listJobs()).filter((job) =>
        ["queued", "running"].includes(job.status),
      ).length >= 2
    )
      throw new Error("The shared runtime already has two active jobs");
    // Recheck after the asynchronous capacity read. This keeps expiry and
    // single-use semantics valid even if a future caller admits concurrently.
    this.#expire();
    const captured = this.#confirmations.get(args.confirmationToken);
    if (!captured)
      throw new Error(
        "Diagnostic AI confirmation expired; preview the report and destination again",
      );
    if (captured.reportId !== args.reportId)
      throw new Error("Diagnostic AI confirmation belongs to another report");
    // Consume before creating or starting any work. An uncertain admission is
    // never replayed through the same user confirmation.
    this.#confirmations.delete(args.confirmationToken);
    const job = await this.runtime.createJob({
      profile: "diagnostic",
      input,
      provider: this.provider,
    });
    return { ...(await this.start(job, captured)), reportId: args.reportId };
  }
  async #owned(args) {
    ownerFields(args);
    const snapshot = await readJobSnapshot(
      this.runtime,
      args.jobId,
      this.hasCurrentJob,
    );
    if (
      snapshot.job.profile !== "diagnostic" ||
      snapshot.job.input?.diagnostic?.reportId !== args.reportId
    )
      throw new Error("Diagnostic task belongs to another report or workflow");
    return snapshot;
  }
  async status(args) {
    const { job, summary } = await this.#owned(args);
    return {
      jobId: job.id,
      reportId: args.reportId,
      status: summary.status,
      output:
        typeof job.result?.text === "string"
          ? job.result.text
          : (job.providerState?.livePi?.output ?? ""),
      error: job.error ?? null,
      updatedAt: job.updatedAt,
    };
  }
  async cancelOwned(args) {
    await this.#owned(args);
    return { cancelled: await this.cancel(args.jobId) };
  }
}
