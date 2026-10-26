/** Trusted launcher contributions have their own lifetimes, independent of
 * Code areas. Hosts place slots; plugins own their components and read state. */
import type { ComponentType } from "react";
import type { LauncherAnnouncementView } from "../launcherDiscoveryTypes";
import type { LauncherPreferences } from "../launcherTypes";
import type { InstanceSnapshotConnection } from "./instanceSnapshots";
import type { BackupRestoreConnection } from "./backupRestore";
import type { InstanceCopyConnection } from "./instanceCopy";

export const launcherSlots = [
  "home.announcements",
  "instance.overview.tools",
  "instance.overview.copy",
] as const;
export type LauncherSlot = (typeof launcherSlots)[number];
export type LauncherSurfaceDeclaration = { id: string; slot: LauncherSlot };
export type AnnouncementConnection = {
  read(refresh: boolean): Promise<LauncherAnnouncementView>;
  openRelease(url: string): Promise<unknown>;
};
export type LauncherSurfaceHost = {
  instanceCopy?: {
    connection: InstanceCopyConnection;
    suggestedName: string;
    available: boolean;
    localization: LauncherPreferences["localization"];
  };
  instanceSnapshots?: {
    connection: InstanceSnapshotConnection;
    restore?: BackupRestoreConnection;
    available: boolean;
    localization: LauncherPreferences["localization"];
  };
  announcements: {
    connection: AnnouncementConnection;
    available: boolean;
    scope: "all" | "important" | "none";
    policy: string;
    localization: LauncherPreferences["localization"];
    notify(message: string): void;
  };
};
export type LauncherSurfaceProps = {
  host: LauncherSurfaceHost;
  /** This activation can be revoked before React has unmounted its component.
   * Check again before side effects and when accepting asynchronous results. */
  lifetime: { isActive(): boolean };
};
export type LauncherSurfaceContribution = LauncherSurfaceDeclaration & {
  owner: string;
  render: ComponentType<LauncherSurfaceProps>;
};
export type RegisteredLauncherSurface = LauncherSurfaceContribution & {
  generation: number;
  lifetime: LauncherSurfaceProps["lifetime"];
};
