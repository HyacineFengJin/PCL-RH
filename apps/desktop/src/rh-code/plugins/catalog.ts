/** Assembly is separate from Vite discovery so tests can supply independent
 * entry modules. Unlisted code never gains executable manifest authority. */
import type {
  BundledPluginEntry,
  PluginRuntime,
  ProductPlugin,
} from "../kernel";
import type { Manifest } from "../types";
export function assemblePlugins(
  manifests: readonly Manifest[],
  entries: readonly BundledPluginEntry[],
  runtime: PluginRuntime = {},
): ProductPlugin[] {
  return manifests.map((source) => {
    // Runtime entries cannot mutate the shared JSON arrays to authorize new
    // IDs. Freeze a private copy so no plugin changes another owner's inputs.
    const manifest: Manifest = { ...source };
    for (const key of [
      "dependencies",
      "capabilities",
      "views",
      "commands",
      "layouts",
    ] as const)
      if (Array.isArray(source[key])) {
        const values = [...source[key]!];
        Object.freeze(values);
        manifest[key] = values;
      }
    Object.freeze(manifest);
    const matches = entries.filter((entry) => entry.id === manifest.id);
    try {
      if (matches.length !== 1)
        throw new Error(
          matches.length
            ? "Duplicate bundled plugin entry"
            : "Bundled plugin entry missing",
        );
      const plugin = matches[0].create(manifest, runtime);
      // Entries receive the authoritative object and may not replace it with
      // their own declarations to expand their permitted contributions.
      if (plugin.manifest !== manifest)
        throw new Error("Bundled entry replaced its authoritative manifest");
      return plugin;
    } catch (error) {
      return {
        manifest,
        loadError: error instanceof Error ? error.message : String(error),
      };
    }
  });
}
