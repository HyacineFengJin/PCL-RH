// The renderer shares one notification stream across API wrappers. No plugin
// content, target identity or permission details cross this local boundary.
const listeners = new Set<() => void>();

export function subscribeExtensionsChanged(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Call only after the host has successfully published a state change. */
export function notifyExtensionsChanged() {
  for (const listener of [...listeners]) listener();
}

// Native has committed a preference change. Consumers must read the latest
// preference store; a queued full snapshot could overwrite a newer local edit.
const preferenceListeners = new Set<() => void>();
export function subscribeLauncherPreferencesApplied(listener: () => void) {
  preferenceListeners.add(listener);
  return () => {
    preferenceListeners.delete(listener);
  };
}
export function notifyLauncherPreferencesApplied() {
  for (const listener of [...preferenceListeners]) listener();
}
// Existing appearance consumers share this stream rather than a second cache.
export const subscribeLauncherAppearanceApplied =
  subscribeLauncherPreferencesApplied;
export const notifyLauncherAppearanceApplied = notifyLauncherPreferencesApplied;
