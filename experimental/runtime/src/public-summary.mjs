import { check } from "./errors.mjs";

const STATES = new Set([
  "queued",
  "running",
  "completed",
  "cancelled",
  "failed",
  "interrupted",
]);
const DOMAIN_TOOLS = new Set([
  "maker.generate",
  "maker.revise",
  "maker.edit_source",
  "porter.inspect",
  "porter.plan",
  "porter.validate",
  "porter.apply_patch",
  "harness.propose_changes",
]);

// A read-only projection for host-owned cards. Never return the internal job.
export function projectPublicJobSummary(job) {
  check(
    typeof job.id === "string" &&
      job.id.length === 36 &&
      /^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/.test(
        job.id,
      ),
    "INVALID_JOB",
    "Invalid job identity",
  );
  check(
    ["maker", "porter", "harness"].includes(job.profile) && STATES.has(job.status),
    "INVALID_JOB",
    "Invalid job profile/status",
  );
  check(
    Number.isSafeInteger(job.revision) && job.revision >= 0,
    "INVALID_JOB",
    "Invalid job revision",
  );
  check(
    typeof job.updatedAt === "string" &&
      job.updatedAt.length === 24 &&
      /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z$/.test(job.updatedAt) &&
      Number.isFinite(Date.parse(job.updatedAt)) &&
      new Date(job.updatedAt).toISOString() === job.updatedAt,
    "INVALID_JOB",
    "Invalid update timestamp",
  );
  let artifactState = "none";
  let blockedReason = null;
  // Only host-owned domain tool receipts determine artifact state. Provider text
  // and generic workspace-write results can never assert an artifact is ready.
  const domain = (job.operations ?? [])
    .filter(
      (op) =>
        op.status === "completed" &&
        DOMAIN_TOOLS.has(op.name) &&
        op.name.startsWith(`${job.profile}.`),
    )
    .at(-1);
  if (
    domain?.name === "maker.generate" &&
    domain.result?.status === "source_generated_only"
  )
    artifactState = "draft_ready";
  if (
    ["maker.revise", "maker.edit_source", "porter.apply_patch"].includes(
      domain?.name,
    ) &&
    domain.result?.status === "reviewed_copy_created"
  )
    artifactState = "draft_ready";
  if (domain?.name.startsWith("porter.")) {
    if (domain.result?.status === "blocked") {
      artifactState = "review_blocked";
      blockedReason = "review_required";
    } else if (domain.result?.status === "review-required")
      artifactState = "draft_ready";
  }
  if (domain?.name === "harness.propose_changes" && domain.result?.status === "pending")
    artifactState = "draft_ready";
  if (job.status === "failed") {
    blockedReason =
      job.error?.code === "BUILD_EXECUTION_DISABLED"
        ? "build_execution_disabled"
        : job.error?.code === "LIVE_MODEL_CALLS_DISABLED"
          ? "live_model_calls_disabled"
          : "runtime_error";
  }
  return {
    schemaVersion: 1,
    jobId: job.id,
    revision: job.revision,
    workflow: job.profile,
    status: job.status,
    artifactState,
    blockedReason,
    verificationState: "not_run",
    updatedAt: job.updatedAt,
  };
}
