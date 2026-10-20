import type {
  ExtensionCardSlot,
  ExperimentalNavigationTarget,
} from "./experimentalTypes";

const settingsSlots: Readonly<Record<string, ExtensionCardSlot | undefined>> =
  Object.freeze({
    launch: "settings.launch",
    java: "settings.java",
    manage: "settings.manage",
    personalize: "settings.cards",
    language: "settings.language",
    misc: "settings.misc",
  });
/** Exact page IDs prevent a plugin area from leaking into log/account/update
 * pages. Unknown pages and navigation targets never select an arbitrary route. */
export function launcherSettingsPluginSlot(
  page: string,
): ExtensionCardSlot | undefined {
  return Object.hasOwn(settingsSlots, page) ? settingsSlots[page] : undefined;
}
export function pluginSettingsDestination(
  target: ExperimentalNavigationTarget,
): string | undefined {
  if (!target.startsWith("settings.")) return;
  const page = target.slice("settings.".length);
  return Object.hasOwn(settingsSlots, page) && page !== "toolbox"
    ? page
    : undefined;
}
