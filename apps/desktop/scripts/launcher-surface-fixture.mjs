/** Actual registry/port implementation; fixture services never call native IPC. */
import { after } from "node:test";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { createRequire } from "node:module";
const esbuild = createRequire(import.meta.url)("esbuild");
const source = new URL("../src/", import.meta.url).pathname;
const scratch = new URL("../../../work/product-plugin-check/", import.meta.url)
  .pathname;
await mkdir(scratch, { recursive: true });
const directory = await mkdtemp(path.join(scratch, "launcher-surfaces-"));
after(() => rm(directory, { recursive: true, force: true }));
const file = path.join(directory, "fixture.mjs");
await esbuild.build({
  stdin: {
    contents: [
      `export { Kernel } from ${JSON.stringify(path.join(source, "rh-code/kernel.ts"))};`,
      `export { assemblePlugins } from ${JSON.stringify(path.join(source, "rh-code/plugins/catalog.ts"))};`,
      `export { createLauncherSurfacePort } from ${JSON.stringify(path.join(source, "productPlugins/surfacePort.ts"))};`,
      `export { LauncherSurfaceBoundary } from ${JSON.stringify(path.join(source, "productPlugins/LauncherSurfaceSlot.tsx"))};`,
      `export { createInstanceSnapshotConnection, snapshotVersion } from ${JSON.stringify(path.join(source, "productPlugins/instanceSnapshots.ts"))};`,
      `export { default as snapshotEntry } from ${JSON.stringify(path.join(source, "rh-code/plugins/launcher.instances.snapshots.plugin.tsx"))};`,
      `export { createBackupRestoreConnection } from ${JSON.stringify(path.join(source, "productPlugins/backupRestore.ts"))};`,
      `export { createInstanceCopyConnection } from ${JSON.stringify(path.join(source, "productPlugins/instanceCopy.ts"))};`,
      `export { default as copyEntry } from ${JSON.stringify(path.join(source, "rh-code/plugins/launcher.instances.copy.plugin.tsx"))};`,
      `export { createClipboardDiscoveryConnection } from ${JSON.stringify(path.join(source, "productPlugins/clipboardDiscovery.ts"))};`,
      `export { default as clipboardEntry } from ${JSON.stringify(path.join(source, "rh-code/plugins/launcher.discovery.clipboard.plugin.tsx"))};`,
    ].join("\n"),
    resolveDir: source,
    loader: "ts",
  },
  bundle: true,
  platform: "node",
  format: "esm",
  outfile: file,
});
const actual = await import(pathToFileURL(file));
export const {
  Kernel,
  assemblePlugins,
  createLauncherSurfacePort,
  LauncherSurfaceBoundary,
  createInstanceSnapshotConnection,
  snapshotVersion,
  snapshotEntry,
  createInstanceCopyConnection,
  createBackupRestoreConnection,
  copyEntry,
  createClipboardDiscoveryConnection,
  clipboardEntry,
} = actual;
export const manifest = (id = "feed", extra = {}) => ({
  id,
  name: id,
  version: "1",
  required: false,
  scope: "launcher",
  dependencies: [],
  capabilities: ["launcher.announcements.read"],
  views: [],
  launcherSurfaces: [{ id: `${id}.component`, slot: "home.announcements" }],
  ...extra,
});
export const plugin = (id = "feed", extra = {}) => ({
  manifest: manifest(id),
  launcherSurfaces: [
    {
      id: `${id}.component`,
      owner: id,
      slot: "home.announcements",
      render: () => null,
    },
  ],
  ...extra,
});
export const records = (kernel) =>
  kernel.listLauncherSurfaces("home.announcements");
export function own(t, definitions) {
  const kernel = new Kernel(definitions);
  t.after(() => kernel.dispose());
  return kernel;
}
export function gate() {
  let resolve;
  const promise = new Promise((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
