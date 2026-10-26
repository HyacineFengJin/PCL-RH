import type { BundledPluginEntry } from "../kernel";
import { SnapshotSurface } from "../../plugins/instances/SnapshotSurface";
import { BackupRestoreSurface } from "../../plugins/instances/BackupRestoreSurface";
import type { LauncherSurfaceProps } from "../../productPlugins/surfaceTypes";
function SnapshotTools(props: LauncherSurfaceProps) {
  return (
    <>
      <SnapshotSurface {...props} />
      <BackupRestoreSurface {...props} />
    </>
  );
}
export default {
  id: "launcher.instances.snapshots",
  create: (manifest, runtime) => ({
    manifest,
    launcherSurfaces: [
      {
        id: "launcher.instances.snapshots.actions",
        owner: manifest.id,
        slot: "instance.overview.tools",
        render: SnapshotTools,
      },
    ],
    apply: async () => {
      await runtime.nativeServices?.activate(manifest.id);
    },
  }),
} satisfies BundledPluginEntry;
