/** Launcher-owned read port. The bundled plugin subscribes only while its
 * Cordis scope is active; no controller or Agent service is imported here. */
import { subscribeExtensionsChanged } from "../extensionEvents";
import { experimentalCall, type ExtensionCatalog } from "../experimentalTypes";
import type { Api } from "../types";

type Surface = { api: Api; native: boolean; visible: boolean };
export function createCommunityCatalogPort(initial: Surface) {
  let surface = initial,
    epoch = 0,
    publishedEpoch = 0,
    stopEvents: (() => void) | undefined;
  const listeners = new Set<() => void>();
  const available = () =>
    surface.native && surface.visible && publishedEpoch === epoch;
  const publish = () => {
    for (const listener of [...listeners]) listener();
  };
  return {
    // Render may retire responses synchronously. Notify in the host's effect so
    // registry listeners never schedule another component during React render.
    setSurface(next: Surface) {
      if (
        next.api !== surface.api ||
        next.native !== surface.native ||
        next.visible !== surface.visible
      ) {
        surface = next;
        epoch++;
      }
    },
    publishSurface() {
      publishedEpoch = epoch;
      publish();
    },
    retire() {
      surface = { ...surface, visible: false };
      epoch++;
      publishedEpoch = epoch;
      publish();
    },
    runtime: {
      isAvailable: available,
      async readCatalog(): Promise<ExtensionCatalog> {
        if (!available()) throw new Error("Plugin catalog is unavailable");
        const owner = epoch,
          api = surface.api;
        const catalog = await experimentalCall<ExtensionCatalog>(
          api,
          "extensions_catalog",
        );
        if (owner !== epoch || !available())
          throw new Error("Plugin catalog request retired");
        return catalog;
      },
      subscribe(listener: () => void) {
        listeners.add(listener);
        if (!stopEvents)
          stopEvents = subscribeExtensionsChanged(() => {
            const surfacePublished = publishedEpoch === epoch;
            epoch++;
            // An ordinary catalog event cannot publish a pending host surface.
            // Its new API/native identity remains closed until the host effect.
            if (surfacePublished) publishedEpoch = epoch;
            publish();
          });
        return () => {
          listeners.delete(listener);
          if (!listeners.size) {
            stopEvents?.();
            stopEvents = undefined;
          }
        };
      },
    },
  };
}
