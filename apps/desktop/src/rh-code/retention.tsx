import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
} from "react";

export const WorkRetention = createContext<() => () => void>(() => () => {});
/** Bind leases to a view mount. Misbehaving or removed views cannot leave the
 * workspace permanently pinned after their component has been disposed. */
export function useMountRetention() {
  const acquire = useContext(WorkRetention),
    owned = useRef(new Set<() => void>()),
    alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      for (const release of owned.current) release();
      owned.current.clear();
    };
  }, []);
  return useCallback(() => {
    if (!alive.current) return () => {};
    const end = acquire();
    const release = () => {
      end();
      owned.current.delete(release);
    };
    owned.current.add(release);
    return release;
  }, [acquire]);
}
export function useRetainedWork(pending: boolean) {
  const acquire = useMountRetention();
  useEffect(() => (pending ? acquire() : undefined), [pending, acquire]);
}
