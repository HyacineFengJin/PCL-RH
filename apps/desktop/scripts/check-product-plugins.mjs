/** Real bundled native entries run through the existing Cordis executor. The
 * native bridge is a deterministic fake; Rust tests exercise actual services. */
import assert from "node:assert/strict";
import { readFile, mkdir, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { createRequire } from "node:module";
const esbuild = createRequire(import.meta.url)("esbuild");
const scratch = new URL("../../../work/product-plugin-check/", import.meta.url);
await mkdir(scratch, { recursive: true });
const directory = await mkdtemp(path.join(scratch.pathname, "entries-"));
try {
  const manifests = JSON.parse(
    await readFile(
      new URL("../../../code/plugins.json", import.meta.url),
      "utf8",
    ),
  ).filter((plugin) => plugin.nativeServices);
  const files = [
    "kernel.ts",
    "plugins/catalog.ts",
    ...manifests.map((plugin) => `plugins/${plugin.id}.plugin.tsx`),
  ];
  await esbuild.build({
    entryPoints: files.map(
      (file) => new URL(`../src/rh-code/${file}`, import.meta.url).pathname,
    ),
    bundle: true,
    platform: "node",
    format: "esm",
    outdir: directory,
    outbase: new URL("../src/rh-code/", import.meta.url).pathname,
    outExtension: { ".js": ".mjs" },
  });
  const { Kernel } = await import(
    pathToFileURL(path.join(directory, "kernel.mjs"))
  );
  const { assemblePlugins } = await import(
    pathToFileURL(path.join(directory, "plugins/catalog.mjs"))
  );
  const imports = [],
    calls = [];
  const entries = Object.fromEntries(
    manifests.map((plugin) => [
      plugin.id,
      [
        async () => {
          imports.push(plugin.id);
          return (
            await import(
              pathToFileURL(
                path.join(directory, `plugins/${plugin.id}.plugin.mjs`),
              )
            )
          ).default;
        },
      ],
    ]),
  );
  const definitions = assemblePlugins(manifests, entries, {
    nativeServices: {
      activate: async (id) => {
        calls.push(id);
      },
    },
  });
  assert.deepEqual(imports, []);
  assert.ok(Object.isFrozen(definitions[0].manifest.nativeServices[0]));
  assert.ok(Object.isFrozen(definitions[0].manifest.runtimeNeeds));
  const kernel = new Kernel(definitions);
  try {
    const disabled = manifests[0].id;
    await kernel.configure([disabled], ["launcher"]);
    assert.deepEqual(
      imports,
      manifests.slice(1).map((plugin) => plugin.id),
    );
    assert.deepEqual(calls, imports);
    assert.equal(kernel.statuses.get(disabled).state, "disabled");
    await kernel.configure([], ["launcher"]);
    assert.equal(kernel.statuses.get(disabled).state, "active");
    const before = calls.length;
    await kernel.configure([], ["launcher", "rh-code"]);
    await kernel.configure([], ["launcher"]);
    assert.equal(calls.length, before);
    assert.ok(
      manifests.every(
        (plugin) => kernel.statuses.get(plugin.id).state === "active",
      ),
    );
  } finally {
    await kernel.dispose();
  }
  const failed = new Kernel(
    assemblePlugins(manifests, entries, {
      nativeServices: {
        activate: async () => {
          throw new Error("native provider unavailable");
        },
      },
    }),
  );
  try {
    await failed.configure([], ["launcher"]);
    assert.ok(
      manifests.every(
        (plugin) => failed.statuses.get(plugin.id).state === "failed",
      ),
    );
  } finally {
    await failed.dispose();
  }
  console.log(
    "Product plugin entries: lazy import, immutable contracts, shared enable/disable, Code retirement and native failure isolation passed",
  );
} finally {
  await rm(directory, { recursive: true, force: true });
}
