import type { BundledPluginEntry } from "../kernel";
import { SnapshotSurface } from "../../plugins/instances/SnapshotSurface";
export default {
  id: "launcher.instances.snapshots",
  create: (manifest) => ({
    manifest,
    launcherSurfaces: [
      {
        id: "launcher.instances.snapshots.actions",
        owner: manifest.id,
        slot: "instance.overview.tools",
        render: SnapshotSurface,
      },
    ],
  }),
} satisfies BundledPluginEntry;
