/** Actual lazy module evaluation, surface retirement and unresolved load races. */
import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { createRequire } from "node:module";
const esbuild = createRequire(import.meta.url)("esbuild");
const scratch = new URL("../../../work/rh-code-check/", import.meta.url)
  .pathname;
await mkdir(scratch, { recursive: true });
const dir = await mkdtemp(path.join(scratch, "scopes-"));
const trackingKey = `__rhPluginScope${Date.now()}`;
try {
  await esbuild.build({
    entryPoints: ["kernel.ts", "plugins/catalog.ts"].map(
      (file) => new URL(`../src/rh-code/${file}`, import.meta.url).pathname,
    ),
    bundle: true,
    platform: "node",
    format: "esm",
    outdir: dir,
    outbase: new URL("../src/rh-code/", import.meta.url).pathname,
    outExtension: { ".js": ".mjs" },
  });
  const { Kernel } = await import(pathToFileURL(path.join(dir, "kernel.mjs")));
  const { assemblePlugins } = await import(
    pathToFileURL(path.join(dir, "plugins/catalog.mjs"))
  );
  const manifest = (id, dependencies = [], declarations = {}) => ({
    id,
    name: id,
    version: "1",
    required: false,
    dependencies,
    views: [],
    capabilities: [],
    ...declarations,
  });
  globalThis[trackingKey] = { imports: [], creates: [], cleanups: [] };
  const tracking = globalThis[trackingKey];
  const scopedManifests = [
    manifest("launcher", [], {
      scope: "launcher",
      commands: ["launcher.command"],
    }),
    manifest("both", ["launcher"], {
      scope: "both",
      commands: ["both.command"],
    }),
    manifest("workspace", ["both"], {
      scope: "rh-code",
      commands: ["workspace.command"],
    }),
    manifest("optional", [], { scope: "rh-code" }),
  ];
  const loaders = {},
    importRequests = [];
  for (const source of scopedManifests) {
    const file = path.join(dir, `scope-${source.id}.mjs`);
    await writeFile(
      file,
      `
      const tracking = globalThis[${JSON.stringify(trackingKey)}];
      tracking.imports.push(${JSON.stringify(source.id)});
      export default {
        id: ${JSON.stringify(source.id)},
        create(manifest) {
          tracking.creates.push(manifest.id);
          const instance = {};
          return {
            manifest,
            commands: (manifest.commands ?? []).map(id => ({
              id, owner: manifest.id, handler: () => instance,
            })),
            apply(ctx) {
              ctx.effect(() => () => tracking.cleanups.push(manifest.id));
            },
          };
        },
      };
    `,
    );
    loaders[source.id] = [
      async () => {
        importRequests.push(source.id);
        return (await import(pathToFileURL(file))).default;
      },
    ];
  }
  const scoped = new Kernel(assemblePlugins(scopedManifests, loaders));
  assert.deepEqual(tracking.imports, []);
  assert.deepEqual(tracking.creates, []);
  await scoped.configure(["optional"], ["launcher"]);
  assert.deepEqual(importRequests, ["launcher", "both"]);
  assert.deepEqual(tracking.imports, ["launcher", "both"]);
  assert.equal(scoped.statuses.get("workspace").state, "dormant");
  assert.equal(scoped.statuses.get("optional").state, "disabled");
  // Disabled-only updates inherit the latest requested scopes, including when
  // several configurations supersede each other before reconciliation runs.
  const rapidUpdates = [
    scoped.configure([]),
    scoped.configure(["both"]),
    scoped.configure(["optional"]),
  ];
  await Promise.all(rapidUpdates);
  assert.equal(scoped.statuses.get("workspace").state, "dormant");
  assert.equal(scoped.statuses.get("launcher").state, "active");
  assert.equal(importRequests.includes("workspace"), false);
  const launcherInstance = scoped.invoke("launcher.command");
  const bothInstance = scoped.invoke("both.command");
  await scoped.configure(["optional"], ["launcher", "rh-code"]);
  const workspaceInstance = scoped.invoke("workspace.command");
  const workspaceCommand = scoped.commands.get("workspace.command");
  assert.equal(scoped.invoke("both.command"), bothInstance);
  await scoped.configure(["optional"], ["launcher"]);
  assert.deepEqual(tracking.cleanups, ["workspace"]);
  assert.throws(() => workspaceCommand(), /unavailable/);
  assert.equal(
    scoped.plugins.find((p) => p.manifest.id === "workspace").commands,
    undefined,
  );
  assert.equal(
    scoped.plugins.find((p) => p.manifest.id === "workspace").apply,
    undefined,
  );
  assert.equal(scoped.invoke("launcher.command"), launcherInstance);
  assert.equal(scoped.invoke("both.command"), bothInstance);
  await scoped.retry();
  assert.equal(scoped.statuses.get("workspace").state, "dormant");
  await scoped.configure(["optional"], ["launcher", "rh-code"]);
  assert.notEqual(scoped.invoke("workspace.command"), workspaceInstance);
  assert.equal(tracking.imports.filter((id) => id === "workspace").length, 1);
  assert.equal(tracking.creates.filter((id) => id === "workspace").length, 2);
  assert.equal(importRequests.includes("optional"), false);
  await scoped.configure(["both", "optional"], ["launcher", "rh-code"]);
  assert.equal(scoped.statuses.get("workspace").state, "blocked");
  assert.equal(scoped.commands.has("workspace.command"), false);
  assert.deepEqual(tracking.cleanups.slice(-2), ["workspace", "both"]);
  assert.equal(scoped.invoke("launcher.command"), launcherInstance);
  await scoped.dispose();

  // Persistent owners cannot force workspace-only imports at launcher startup.
  let forbiddenImports = 0;
  const isolation = new Kernel(
    assemblePlugins(
      [
        manifest("rh-only", [], { scope: "rh-code" }),
        manifest("launcher-bad", ["rh-only"], { scope: "launcher" }),
        manifest("both-bad", ["rh-only"], { scope: "both" }),
        manifest("invalid-scope", [], { scope: "other" }),
        manifest("identity", [], { scope: "launcher" }),
      ],
      Object.fromEntries(
        [
          "rh-only",
          "launcher-bad",
          "both-bad",
          "invalid-scope",
          "identity",
        ].map((id) => [
          id,
          [
            async () => {
              if (id !== "identity") forbiddenImports++;
              return { id: "wrong", create: (manifest) => ({ manifest }) };
            },
          ],
        ]),
      ),
    ),
  );
  await isolation.configure([], ["launcher"]);
  assert.equal(forbiddenImports, 0);
  for (const id of ["launcher-bad", "both-bad", "invalid-scope", "identity"])
    assert.equal(isolation.statuses.get(id).state, "failed");
  assert.match(isolation.statuses.get("launcher-bad").error, /cannot depend/);
  assert.match(isolation.statuses.get("identity").error, /identity mismatch/);
  assert.equal(isolation.statuses.get("rh-only").state, "dormant");
  await isolation.dispose();

  // Closing the host or leaving a surface during an import prevents late factories.
  for (const close of [true, false]) {
    let releaseImport, importStarted;
    const entered = new Promise((resolve) => {
      importStarted = resolve;
    });
    const pendingImport = new Promise((resolve) => {
      releaseImport = resolve;
    });
    let lateCreates = 0;
    const loading = new Kernel(
      assemblePlugins([manifest("loading")], {
        loading: [
          async () => {
            importStarted();
            await pendingImport;
            return {
              id: "loading",
              create: (manifest) => {
                lateCreates++;
                return { manifest };
              },
            };
          },
        ],
      }),
    );
    const activation = loading.configure([], ["rh-code"]);
    await entered;
    const finish = close
      ? loading.dispose()
      : loading.configure([], ["launcher"]);
    releaseImport();
    await Promise.all([activation, finish]);
    assert.equal(lateCreates, 0);
    assert.equal(
      loading.statuses.get("loading").state,
      close ? "stopped" : "dormant",
    );
    await loading.dispose();
  }
  // Closing during async apply disposes the published command and effect once.
  let applyStarted,
    releaseApply,
    cleanupDuringApply = 0;
  const enteredApply = new Promise((resolve) => {
    applyStarted = resolve;
  });
  const pendingApply = new Promise((resolve) => {
    releaseApply = resolve;
  });
  const closingApply = new Kernel(
    assemblePlugins(
      [manifest("closing", [], { commands: ["closing.command"] })],
      [
        {
          id: "closing",
          create: (manifest) => ({
            manifest,
            commands: [
              { id: "closing.command", owner: "closing", handler: () => true },
            ],
            apply: async (ctx) => {
              ctx.effect(() => () => cleanupDuringApply++);
              applyStarted();
              await pendingApply;
            },
          }),
        },
      ],
    ),
  );
  const activation = closingApply.configure([]);
  await enteredApply;
  const stale = closingApply.commands.get("closing.command");
  const shutdown = closingApply.dispose();
  releaseApply();
  await Promise.all([activation, shutdown]);
  assert.equal(cleanupDuringApply, 1);
  assert.throws(() => stale(), /unavailable/);
  assert.equal(closingApply.plugins[0].commands, undefined);
  assert.equal(closingApply.statuses.get("closing").state, "stopped");
  console.log(
    "RH Code: lazy module evaluation, surface scopes, retained shared instances, retirement/reentry, dependency authority and dispose/load races passed.",
  );
} finally {
  delete globalThis[trackingKey];
  await rm(dir, { recursive: true, force: true });
}
