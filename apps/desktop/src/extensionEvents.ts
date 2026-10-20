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

// Native has committed appearance changes. Consumers must read the latest
// preference store; a queued full snapshot could overwrite a newer local edit.
const appearanceListeners = new Set<() => void>();
export function subscribeLauncherAppearanceApplied(listener: () => void) {
  appearanceListeners.add(listener);
  return () => {
    appearanceListeners.delete(listener);
  };
}
export function notifyLauncherAppearanceApplied() {
  for (const listener of [...appearanceListeners]) listener();
}
