/** One draft owns one volatile import token. Releasing it never mutates source,
 * durable projects or admitted jobs. Each receipt keeps its original transport. */
import type { Api } from "./types";
export type SourceRelease = (sourceId: string) => Promise<unknown>;
export const sourceToken = (value: unknown): value is string =>
  typeof value === "string" &&
  /^[a-f0-9]{8}-(?:[a-f0-9]{4}-){3}[a-f0-9]{12}$/.test(value);

// This closure captures only its transport, outside the large controller scope.
export function createSourceRelease(api: Api): SourceRelease {
  return (sourceId) => {
    if (!sourceToken(sourceId))
      return Promise.reject(new Error("Invalid source release token"));
    return api<boolean>("experimental_call", {
      operation: "source_release",
      args: { sourceId },
    });
  };
}

export function createPorterSourceOwner() {
  let current: { id: string; release: SourceRelease } | null = null;
  let disposed = false;
  const discard = (id: string, release: SourceRelease): Promise<void> => {
    if (!sourceToken(id)) return Promise.resolve();
    return Promise.resolve()
      .then(() => release(id))
      .then(
        () => {},
        () => {},
      );
  };
  const clear = (): Promise<void> => {
    const previous = current;
    current = null;
    return previous
      ? discard(previous.id, previous.release)
      : Promise.resolve();
  };
  return {
    adopt(id: string, release: SourceRelease): boolean {
      if (!sourceToken(id)) throw new Error("Invalid imported source token");
      if (disposed) {
        void discard(id, release);
        return false;
      }
      if (current?.id === id) return true;
      void clear();
      current = { id, release };
      return true;
    },
    clear,
    discard,
    dispose(): Promise<void> {
      disposed = true;
      return clear();
    },
  };
}
export type PorterSourceOwner = ReturnType<typeof createPorterSourceOwner>;
