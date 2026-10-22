/** The shell supplies placement and a narrow service host. Components, request
 * state and failures belong to the registered activation, not to the shell. */
import { Component, useSyncExternalStore, type ReactNode } from "react";
import type { LauncherEffect } from "../launcherTypes";
import type { LauncherSurfacePort } from "./surfacePort";
import type {
  LauncherSlot,
  LauncherSurfaceHost,
  RegisteredLauncherSurface,
} from "./surfaceTypes";

export class LauncherSurfaceBoundary extends Component<
  {
    port: LauncherSurfacePort;
    surface: RegisteredLauncherSurface;
    children: ReactNode;
  },
  { failed: boolean }
> {
  state = { failed: false };
  static getDerivedStateFromError() {
    return { failed: true };
  }
  componentDidCatch(error: unknown) {
    // The port checks the exact registration. A delayed boundary from an old
    // host cannot fail the replacement activation that happens to share an ID.
    void this.props.port.reportFailure(this.props.surface, error);
  }
  render() {
    return this.state.failed ? null : this.props.children;
  }
}

export function LauncherSurfaceSlot({
  port,
  slot,
  host,
}: {
  port: LauncherSurfacePort;
  slot: LauncherSlot;
  host: LauncherSurfaceHost;
}) {
  useSyncExternalStore(port.subscribe, port.revision, port.revision);
  return (
    <>
      {port.list(slot).map((surface) => {
        const Render = surface.render;
        return (
          <LauncherSurfaceBoundary
            key={surface.generation}
            port={port}
            surface={surface}
          >
            <Render host={host} lifetime={surface.lifetime} />
          </LauncherSurfaceBoundary>
        );
      })}
    </>
  );
}

const announcementEffects: readonly LauncherEffect[] = ["announcements"];
const noEffects: readonly LauncherEffect[] = [];
export function useLauncherSurfaceEffects(port: LauncherSurfacePort) {
  // A Code registry update does not need to rerender the entire launcher when
  // the set of supported launcher effects has not changed.
  const effects = useSyncExternalStore(
    port.subscribe,
    () => port.effects().join(","),
    () => "",
  );
  return effects ? announcementEffects : noEffects;
}
