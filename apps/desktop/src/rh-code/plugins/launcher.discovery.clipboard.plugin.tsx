import type { BundledPluginEntry } from "../kernel";
import { ClipboardDiscoverySurface } from "../../plugins/discovery/ClipboardDiscoverySurface";
export default {
  id: "launcher.discovery.clipboard",
  create: (manifest, runtime) => ({
    manifest,
    launcherSurfaces: [
      {
        id: "launcher.discovery.clipboard.focus",
        owner: manifest.id,
        slot: "launcher.discovery",
        render: ClipboardDiscoverySurface,
      },
    ],
    apply: async () => {
      await runtime.nativeServices?.activate(manifest.id);
    },
  }),
} satisfies BundledPluginEntry;
