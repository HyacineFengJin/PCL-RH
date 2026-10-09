/** Threads reference shared Pi jobs. UI selections are scope hints, never file
 * permissions; unit approvals are staged until the host commits one digest. */
import type { JobSummary, ReviewView } from "./experimentalTypes";
import type { MakerUnit } from "./makerWorkspaceState";
export type MakerMode = "ask" | "plan" | "execute";
export type MakerRound = {
  jobId: string;
  request: string;
  mode: MakerMode;
  checkpointId: string | null;
  scopes?: MakerScopeHint[];
};
export type MakerThread = {
  id: string;
  projectId: string | null;
  name: string;
  notes: string;
  title: string;
  createdAt: string;
  updatedAt: string;
  rounds: MakerRound[];
  summary?: JobSummary | null;
};
export type MakerActivity = {
  id: string;
  name: string;
  status: string;
  path?: string;
  steps?: string[];
};
export type MakerScopeHint = {
  id: string;
  name: string;
  kind: MakerUnit["kind"];
  files: string[];
  notes: string;
};
export function threadGroup(
  thread: MakerThread,
  jobs: readonly JobSummary[],
  pendingJobs: ReadonlySet<string>,
) {
  if (thread.summary && ["queued", "running"].includes(thread.summary.status))
    return "running";
  if (
    thread.rounds.some((round) =>
      jobs.some(
        (job) =>
          job.jobId === round.jobId &&
          ["queued", "running"].includes(job.status),
      ),
    )
  )
    return "running";
  if (thread.rounds.some((round) => pendingJobs.has(round.jobId)))
    return "review";
  return "completed";
}
type MakerDiff = { path: string; diff?: string; unified_diff?: string };
export function groupReview(
  review: ReviewView,
  units: readonly MakerScopeHint[],
) {
  const changes: MakerDiff[] =
    review.preview.diffs || review.preview.changes || [];
  const groups = new Map<
    string,
    { id: string; name: string; changes: MakerDiff[] }
  >();
  for (const change of changes) {
    const owners = units.filter((unit) => unit.files.includes(change.path));
    // Ambiguous/unindexed paths stay together; guessed ownership cannot become
    // consent to edit another unit. The host still applies the entire digest.
    const owner = owners.length === 1 ? owners[0] : null;
    const id = owner?.id ?? "unassigned";
    if (!groups.has(id))
      groups.set(id, { id, name: owner?.name ?? "", changes: [] });
    groups.get(id)!.changes.push(change);
  }
  if (!groups.size)
    groups.set("unassigned", { id: "unassigned", name: "", changes: [] });
  return [...groups.values()];
}
export function scopePrompt(request: string, units: readonly MakerScopeHint[]) {
  const value = [
    request,
    ...units.map(
      (unit) =>
        `Referenced unit: ${unit.kind}/${unit.name}\nRequirements: ${unit.notes}\nRelated source paths:\n${unit.files.join("\n")}`,
    ),
    "Referenced units bound the requested work. Explain any necessary changes outside this scope before proposing them. Preserve handwritten protection.",
  ].join("\n\n");
  if (value.length > 12000)
    throw new Error("Scope and request exceed 12000 characters");
  return value;
}
