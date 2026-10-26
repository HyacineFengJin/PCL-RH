import type { BundledPluginEntry } from "../kernel";
import { CopySurface } from "../../plugins/instances/CopySurface";
export default {
  id: "launcher.instances.copy",
  create: (manifest, runtime) => ({
    manifest,
    launcherSurfaces: [
      {
        id: "launcher.instances.copy.actions",
        owner: manifest.id,
        slot: "instance.overview.copy",
        render: CopySurface,
      },
    ],
    apply: async () => {
      await runtime.nativeServices?.activate(manifest.id);
    },
  }),
} satisfies BundledPluginEntry;
