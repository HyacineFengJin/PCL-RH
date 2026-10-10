/** Runs the real Cordis registry and immutable layout code. No launcher data,
 * native host, browser clipboard, AI provider or game process is used. */
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { createRequire } from "node:module";
const require = createRequire(import.meta.url),
  esbuild = require("esbuild"),
  dir = await mkdtemp(path.join(tmpdir(), "rh-code-registry-"));
try {
  await esbuild.build({
    entryPoints: [
      new URL("../src/rh-code/kernel.ts", import.meta.url).pathname,
      new URL("../src/rh-code/layout.ts", import.meta.url).pathname,
    ],
    bundle: true,
    platform: "node",
    format: "esm",
    outdir: dir,
    outExtension: { ".js": ".mjs" },
  });
  const { Kernel } = await import(pathToFileURL(path.join(dir, "kernel.mjs"))),
    layout = await import(pathToFileURL(path.join(dir, "layout.mjs")));
  const manifest = (id, deps = []) => ({
    id,
    name: id,
    version: "1",
    required: false,
    dependencies: deps,
    views: [],
    capabilities: [],
  });
  let effects = 0,
    k;
  const leaf = {
    manifest: manifest("view", ["core"]),
    views: [
      {
        id: "chat",
        owner: "view",
        label: "Chat",
        render: () => null,
        icon: () => null,
      },
    ],
    apply: (ctx) => {
      ctx.effect(() => {
        effects++;
        return () => {
          effects--;
        };
      });
      k.registerCommand(ctx, "test", () => 42);
    },
  };
  k = new Kernel([leaf, { manifest: manifest("core") }]);
  await k.configure([]);
  assert.equal(k.views.size, 1);
  assert.equal(effects, 1);
  assert.equal(k.invoke("test"), 42);
  await k.configure(["view"]);
  assert.equal(k.views.size, 0);
  assert.equal(effects, 0);
  assert.throws(() => k.invoke("test"), /unavailable/);
  await k.configure([]);
  assert.equal(k.views.size, 1);
  assert.equal(effects, 1);
  await k.dispose();
  assert.equal(effects, 0);
  assert.equal(k.views.size, 0);
  assert.throws(
    () =>
      new Kernel([
        { manifest: manifest("a", ["b"]) },
        { manifest: manifest("b", ["a"]) },
      ]),
    /cyclic/,
  );
  const a = layout.area("chat", "one"),
    b = layout.area("source");
  b.file = "Main.java";
  const tree = layout.split("row", a, b);
  const swapped = layout.exchange(tree, a.id, b.id);
  assert.equal(swapped.children[0].file, "Main.java");
  assert.equal(swapped.children[1].conversation, "one");
  assert.equal(new Set(layout.leaves(swapped).map((v) => v.id)).size, 2);
  assert.equal(tree.children[0].conversation, "one");
  assert.equal(layout.remove(swapped, b.id).conversation, "one");
  assert.equal(layout.resize(tree, tree.id, 10).ratio, 0.85);
  assert.equal(layout.depth(tree), 1);
  let deep = layout.area("chat");
  for (let n = 0; n < 8; n++)
    deep = layout.split("row", deep, layout.area("source"));
  assert.equal(layout.depth(deep), 8);
  // A failed module must unregister its earlier effects and not poison the queue.
  let fail = true;
  const bad = new Kernel([
    {
      manifest: manifest("bad"),
      apply: (ctx) => {
        ctx.effect(() => {
          effects++;
          return () => {
            effects--;
          };
        });
        if (fail) throw new Error("broken module");
      },
    },
  ]);
  await assert.rejects(bad.configure([]));
  assert.equal(effects, 0);
  fail = false;
  await bad.configure([]);
  assert.equal(effects, 1);
  await bad.dispose();
  assert.equal(effects, 0);
  console.log(
    "RH Code: Cordis lifecycle, commands, dependency ordering, failure recovery and layout identity passed.",
  );
} finally {
  await rm(dir, { recursive: true, force: true });
}
