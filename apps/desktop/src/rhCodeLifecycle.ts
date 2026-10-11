/** Browser modules stay cached after import. This policy retires live services,
 * views and their buffers; it never treats a silent running job as idle. */
export const CODE_AWAY_MS = 10 * 60_000;
export const CODE_IDLE_MS = 5 * 60_000;
export function maySuspendCode(
  now: number,
  awaySince: number | null,
  state: {
    busy: number;
    activeJobs: number;
    lastActivityAt: number;
  },
  leases: number,
) {
  return (
    awaySince !== null &&
    now - awaySince >= CODE_AWAY_MS &&
    now - state.lastActivityAt >= CODE_IDLE_MS &&
    !state.busy &&
    !state.activeJobs &&
    !leases
  );
}
export function createWorkLeases() {
  const holders = new Set<symbol>();
  return {
    get size() {
      return holders.size;
    },
    acquire() {
      const id = Symbol();
      holders.add(id);
      return () => {
        holders.delete(id);
      };
    },
  };
}
