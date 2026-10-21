/** WebUI part of a native product plugin. Presentation stays in the existing
 * homepage; this entry joins the same catalog, activation and Plugin manager. */
import type { BundledPluginEntry } from "../kernel";
export default {
  id: "launcher.home.announcements",
  create: (manifest, runtime) => ({
    manifest,
    apply: async () => {
      await runtime.nativeServices?.activate(manifest.id);
    },
  }),
} satisfies BundledPluginEntry;
