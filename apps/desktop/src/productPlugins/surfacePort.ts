/** One lightweight connection to the existing product kernel. Importing the
 * launcher surface neither loads a Code controller nor creates a second kernel. */
import type { Kernel } from "../rh-code/kernel";
import type { LauncherEffect } from "../launcherTypes";
import type { LauncherSlot, RegisteredLauncherSurface } from "./surfaceTypes";

let nextMountGeneration = 0;

export function createLauncherSurfacePort() {
  type Binding = {
    kernel: Kernel;
    unsubscribe(): void;
    records: Map<RegisteredLauncherSurface, RegisteredLauncherSurface>;
  };
  let current: Binding | null = null;
  let revision = 0;
  const listeners = new Set<() => void>();
  const changed = () => {
    revision++;
    for (const listener of listeners) listener();
  };
  return {
    attach(kernel: Kernel) {
      current?.unsubscribe();
      current?.records.clear();
      const binding: Binding = {
        kernel,
        unsubscribe: () => {},
        records: new Map(),
      };
      binding.unsubscribe = kernel.subscribe(() => {
        for (const source of binding.records.keys())
          if (!source.lifetime.isActive()) binding.records.delete(source);
        changed();
      });
      current = binding;
      changed();
      return () => {
        binding.unsubscribe();
        binding.records.clear();
        if (current !== binding) return;
        current = null;
        changed();
      };
    },
    subscribe(listener: () => void) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    revision: () => revision,
    list(slot: LauncherSlot): RegisteredLauncherSurface[] {
      const binding = current;
      if (!binding) return [];
      return binding.kernel.listLauncherSurfaces(slot).map((source) => {
        let record = binding.records.get(source);
        if (!record) {
          // Detaching a host revokes its saved actions immediately, even while
          // the kernel is awaiting an unrelated asynchronous cleanup.
          record = Object.freeze({
            ...source,
            generation: ++nextMountGeneration,
            lifetime: Object.freeze({
              isActive: () => current === binding && source.lifetime.isActive(),
            }),
          });
          binding.records.set(source, record);
        }
        return record;
      });
    },
    effects(): LauncherEffect[] {
      return current?.kernel.listLauncherSurfaces("home.announcements").length
        ? ["announcements"]
        : [];
    },
    reportFailure(record: RegisteredLauncherSurface, error: unknown) {
      const binding = current;
      if (
        !binding ||
        !record.lifetime.isActive() ||
        !binding.kernel
          .listLauncherSurfaces(record.slot)
          .some((source) => binding.records.get(source) === record)
      )
        return Promise.resolve();
      return binding.kernel.reportFailure(record.owner, error);
    },
  };
}
export type LauncherSurfacePort = ReturnType<typeof createLauncherSurfacePort>;
