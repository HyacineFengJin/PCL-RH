import type { BundledPluginEntry } from "../kernel";
/** The Rust part owns the actual analyzer. No replacement UI is introduced. */
export default {
  id: "launcher.diagnostics.analysis",
  create: (manifest, runtime) => ({
    manifest,
    apply: async () => {
      await runtime.nativeServices?.activate(manifest.id);
    },
  }),
} satisfies BundledPluginEntry;
