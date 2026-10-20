/** Vite emits independent trusted entry chunks. Discovery only records loaders;
 * Kernel imports an entry when its declared lifetime is active and enabled. */
import manifests from "../../../../../code/plugins.json";
import type {
  BundledPluginEntry,
  Kernel,
  LazyProductPlugin,
  PluginRuntime,
} from "../kernel";
import type { Manifest } from "../types";
import { assemblePlugins, type PluginEntryLoaders } from "./catalog";
const modules = import.meta.glob<BundledPluginEntry>("./*.plugin.tsx", {
  import: "default",
});
// Existing first-party files use `<name>.plugin.tsx` for `rh.<name>`.
// Other namespaces use their complete manifest ID as the filename.
const entries: Record<string, (() => Promise<BundledPluginEntry>)[]> = {};
for (const [path, load] of Object.entries(modules)) {
  const name = path.slice(2, -".plugin.tsx".length);
  const ids = name.includes(".") ? [name] : [name, `rh.${name}`];
  for (const id of ids) (entries[id] ??= []).push(load);
}
export function productPlugins(
  commands?: PluginRuntime["commands"],
  _owner?: () => Kernel,
  keepAwake?: PluginRuntime["keepAwake"],
  navigate?: PluginRuntime["navigate"],
  community?: PluginRuntime["community"],
): LazyProductPlugin[] {
  return assemblePlugins(
    manifests as Manifest[],
    entries as PluginEntryLoaders,
    {
      commands,
      keepAwake,
      navigate,
      community,
    },
  );
}
