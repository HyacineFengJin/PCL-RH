/** Retirement is a host ownership decision. Activity output and UI polling are
 * not evidence that a job stopped: in-memory runs, child ownership and pending
 * capabilities pin the process. Durable pending reviews conservatively pin too.
 * Native stdio serializes this probe with every dispatch and rechecks admission
 * before shutdown. This probe never cancels or mutates a job. */
import fs from "node:fs/promises";
import path from "node:path";

export async function prepareIdle({
  runtime,
  reviews,
  children,
  pendingJobs,
  liveRuns,
  pendingExtensions,
  sourceImports,
  safeMode,
  selection,
  preset,
}) {
  const busy = () => ({ busy: true });
  if (
    runtime.hasActiveWork() ||
    pendingJobs.size ||
    liveRuns.size ||
    pendingExtensions.size ||
    sourceImports.size ||
    safeMode ||
    children.some((child) => child.hasActiveWork())
  )
    return busy();
  // Rust retains only this durable identity for wakeup, never the selected key.
  if (
    selection &&
    (!preset || JSON.stringify(selection) !== JSON.stringify(preset.selection))
  )
    return busy();
  try {
    const ids = (await fs.readdir(runtime.store.root)).filter((id) =>
      /^[a-f0-9-]{36}$/.test(id),
    );
    // Unlike status' display history, this scan is deliberately not capped at
    // 100 jobs: legacy Maker/Porter reviews must not disappear from retirement.
    for (const id of ids) {
      const names = await fs
        .readdir(
          path.join(runtime.store.directory(id), "review-control/reviews"),
        )
        .catch((error) => {
          if (error.code === "ENOENT") return [];
          throw error;
        });
      for (const name of names.filter((name) => name.endsWith(".json"))) {
        const review = await reviews.getReview(id, name.slice(0, -5));
        if (["pending", "approved", "applying"].includes(review.status))
          return busy();
      }
    }
  } catch {
    // Unreadable review state cannot authorize destruction of process context.
    return busy();
  }
  // Recheck background owners after asynchronous filesystem inspection.
  if (
    runtime.hasActiveWork() ||
    pendingJobs.size ||
    liveRuns.size ||
    children.some((child) => child.hasActiveWork())
  )
    return busy();
  return { busy: false, resumePresetId: selection ? preset.id : null };
}
