/** Vite bundles these trusted entries at compile time. Adding a first-party
 * component requires its own entry and an authoritative shared manifest. */
import manifests from "../../../../../code/plugins.json";
import type {
  BundledPluginEntry,
  Kernel,
  PluginRuntime,
  ProductPlugin,
} from "../kernel";
import type { Manifest } from "../types";
import { assemblePlugins } from "./catalog";
const entries = import.meta.glob<BundledPluginEntry>("./*.plugin.tsx", {
  eager: true,
  import: "default",
});
export function productPlugins(
  commands?: PluginRuntime["commands"],
  _owner?: () => Kernel,
): ProductPlugin[] {
  return assemblePlugins(manifests as Manifest[], Object.values(entries), {
    commands,
  });
}
