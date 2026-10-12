/** Discovery supplies loaders keyed by authoritative manifest ID. Merely listing
 * metadata never evaluates entry modules or creates their runtime instances. */
import type {
  BundledPluginEntry,
  LazyProductPlugin,
  PluginRuntime,
} from "../kernel";
import type { Manifest } from "../types";
export type PluginEntryLoaders = Readonly<
  Record<string, readonly (() => Promise<BundledPluginEntry>)[]>
>;
export function assemblePlugins(
  manifests: readonly Manifest[],
  entries: PluginEntryLoaders | readonly BundledPluginEntry[],
  runtime: PluginRuntime = {},
): LazyProductPlugin[] {
  return manifests.map((source) => {
    // Runtime entries cannot mutate shared JSON arrays to authorize new IDs.
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
    if (Array.isArray(source.tools)) {
      manifest.tools = source.tools.map((tool) => {
        const modes = [...tool.modes];
        Object.freeze(modes);
        return Object.freeze({ ...tool, modes });
      });
      Object.freeze(manifest.tools);
    }
    Object.freeze(manifest);
    // The array form supports already imported entries supplied by tests/hosts.
    // Production always supplies lazy module loaders.
    const matches = Array.isArray(entries)
      ? entries
          .filter((entry) => entry.id === manifest.id)
          .map((entry) => async () => entry)
      : ((entries as PluginEntryLoaders)[manifest.id] ?? []);
    return {
      manifest,
      runtime,
      load: async () => {
        if (matches.length !== 1)
          throw new Error(
            matches.length
              ? "Duplicate bundled plugin entry"
              : "Bundled plugin entry missing",
          );
        return matches[0]();
      },
    };
  });
}
