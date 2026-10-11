/** Admission counters live outside React scheduling: queued requests pin the
 * service before their first await. Edits advance a generation so suspension
 * cannot acknowledge newer work with an older save. */
export type LifecycleState = Readonly<{
  busy: number;
  dirty: boolean;
  activeJobs: number;
  lastActivityAt: number;
  suspended: boolean;
}>;
export type WorkKind =
  "mutation" | "source" | "read" | "operation" | "external";
export function createControllerLifecycle() {
  const pending: Record<WorkKind, number> = {
    mutation: 0,
    source: 0,
    read: 0,
    operation: 0,
    external: 0,
  };
  let generation = 0,
    lastActivityAt = Date.now(),
    suspended = false;
  return {
    pending,
    get generation() {
      return generation;
    },
    get suspended() {
      return suspended;
    },
    touch() {
      lastActivityAt = Date.now();
    },
    edited() {
      generation++;
      lastActivityAt = Date.now();
    },
    begin(kind: WorkKind, meaningful = true) {
      pending[kind]++;
      if (meaningful) lastActivityAt = Date.now();
      let ended = false;
      return () => {
        if (ended) return;
        ended = true;
        pending[kind]--;
        if (meaningful) lastActivityAt = Date.now();
      };
    },
    state(dirty: boolean, activeJobs: number): LifecycleState {
      return Object.freeze({
        busy: Object.values(pending).reduce((sum, count) => sum + count, 0),
        dirty,
        activeJobs,
        lastActivityAt,
        suspended,
      });
    },
    suspend() {
      suspended = true;
    },
  };
}
