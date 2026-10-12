/** A public status and its result must describe the same disk generation.
 * Re-reading a summary after loading the job can observe completion with the
 * preceding null result, causing the UI to persist an empty terminal message. */
import { check } from "./runtime/src/errors.mjs";
import { projectPublicJobSummary } from "./runtime/src/public-summary.mjs";

export async function readJobSnapshot(runtime, jobId, hasCurrentJob) {
  const job = await runtime.getJob(jobId);
  check(job.id === jobId, "JOB_IDENTITY_CHANGED", "Task snapshot identity differs from the requested task");
  const summary = projectPublicJobSummary(job);
  if (["queued", "running"].includes(job.status) && !hasCurrentJob(job.id))
    summary.status = "interrupted";
  return { job, summary };
}
