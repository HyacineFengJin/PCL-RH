/** Real Cordis activations and publisher retirement; no native/AI calls. */
import assert from "node:assert/strict";
import { after, test } from "node:test";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { createRequire } from "node:module";
const esbuild = createRequire(import.meta.url)("esbuild");
const scratch = new URL("../../../work/rh-code-check/", import.meta.url)
  .pathname;
await mkdir(scratch, { recursive: true });
const dir = await mkdtemp(path.join(scratch, "dynamic-views-"));
after(() => rm(dir, { recursive: true, force: true }));
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
const manifest = (id = "panels", extra = {}) => ({
  id,
  name: id,
  version: "1",
  required: false,
  dependencies: [],
  capabilities: [],
  views: [],
  dynamicViews: { prefix: `${id}.panel.`, limit: 3 },
  ...extra,
});
const view = (id, owner = "panels", extra = {}) => ({
  id,
  owner,
  label: id,
  icon: () => null,
  render: () => null,
  ...extra,
});
const gate = () => {
  let resolve;
  const promise = new Promise((done) => (resolve = done));
  return { promise, resolve };
};
const ids = (kernel) => [...kernel.views.keys()].sort();
function own(t, definitions) {
  const kernel = new Kernel(definitions);
  t.after(() => kernel.dispose());
  return kernel;
}

await test("dynamic authority is validated before lazy module import", async (t) => {
  let imports = 0;
  const invalid = [
    null,
    [],
    {},
    { prefix: "panels.panel.", limit: 1, extra: true },
    { prefix: "foreign.panel.", limit: 1 },
    { prefix: "panelss.panel.", limit: 1 },
    { prefix: "panels.panel", limit: 1 },
    { prefix: "panels.中.", limit: 1 },
    { prefix: "panels." + "a".repeat(73) + ".", limit: 1 },
    ...[0, 385, -1, 1.5, NaN, "3"].map((limit) => ({
      prefix: "panels.panel.",
      limit,
    })),
  ];
  for (const dynamicViews of invalid) {
    const kernel = own(
      t,
      assemblePlugins([manifest("panels", { dynamicViews })], {
        panels: [
          async () => {
            imports++;
            return { id: "panels", create: (manifest) => ({ manifest }) };
          },
        ],
      }),
    );
    await kernel.configure([]);
    assert.equal(kernel.statuses.get("panels").state, "failed");
    assert.match(
      kernel.statuses.get("panels").error,
      /dynamic view declaration/,
    );
  }
  assert.equal(imports, 0);
  for (const dynamicViews of [
    { prefix: "panels.", limit: 1 },
    { prefix: "panels.panel.", limit: 384 },
    { prefix: "panels." + "a".repeat(72) + ".", limit: 3 },
  ]) {
    const kernel = own(t, [{ manifest: manifest("panels", { dynamicViews }) }]);
    await kernel.configure([]);
    assert.equal(kernel.statuses.get("panels").state, "active");
  }
});

await test("catalog copies and freezes dynamic authority without eager import", async (t) => {
  const source = manifest();
  let creates = 0;
  const definitions = assemblePlugins(
    [source],
    [
      {
        id: "panels",
        create(manifest) {
          creates++;
          return { manifest };
        },
      },
    ],
  );
  assert.notEqual(definitions[0].manifest.dynamicViews, source.dynamicViews);
  assert.ok(Object.isFrozen(definitions[0].manifest.dynamicViews));
  assert.throws(() => {
    definitions[0].manifest.dynamicViews.limit = 384;
  }, TypeError);
  source.dynamicViews.prefix = "foreign.";
  source.dynamicViews.limit = 384;
  assert.deepEqual(definitions[0].manifest.dynamicViews, {
    prefix: "panels.panel.",
    limit: 3,
  });
  const kernel = own(t, definitions);
  await kernel.configure([], ["launcher"]);
  assert.equal(creates, 0);
  await kernel.configure([], ["rh-code"]);
  assert.equal(creates, 1);
  assert.equal(kernel.statuses.get("panels").state, "active");
});

await test("replacement publishes once, retains statics and snapshots descriptor values", async (t) => {
  let publish;
  const staticView = view("panels.static");
  const kernel = own(t, [
    {
      manifest: manifest("panels", { views: [staticView.id] }),
      views: [staticView],
      apply(_ctx, { replaceViews }) {
        publish = replaceViews;
      },
    },
  ]);
  await kernel.configure([]);
  const publications = [];
  kernel.subscribe(() => publications.push(ids(kernel)));
  const first = view("panels.panel.first");
  const second = view("panels.panel.second");
  publish([first, second]);
  assert.deepEqual(publications, [[first.id, second.id, staticView.id].sort()]);
  assert.equal(kernel.views.get(first.id).render, first.render);
  assert.ok(Object.isFrozen(kernel.views.get(first.id)));
  const third = view("panels.panel.third");
  const update = { ...second, label: "Updated title" };
  const next = [update, third];
  publish(next);
  assert.deepEqual(
    publications.at(-1),
    [second.id, third.id, staticView.id].sort(),
  );
  assert.equal(publications.length, 2);
  assert.equal(kernel.views.get(second.id).render, second.render);
  update.label = "Mutated caller value";
  update.id = "panels.panel.changed";
  next.length = 0;
  assert.equal(kernel.views.get(second.id).label, "Updated title");
  assert.equal(kernel.views.get(staticView.id), staticView);
  await kernel.configure([]);
  // A harmless configuration reconciliation keeps the active publisher alive.
  publish([]);
  assert.deepEqual(ids(kernel), [staticView.id]);
});

await test("invalid batches preserve the entire previous publication", async (t) => {
  let publish;
  const kernel = own(t, [
    {
      manifest: manifest("panels", { views: ["panels.panel.static"] }),
      views: [view("panels.panel.static")],
      apply(_ctx, services) {
        publish = services.replaceViews;
      },
    },
    {
      manifest: manifest("inactive", { views: ["panels.panel.reserved"] }),
    },
  ]);
  await kernel.configure(["inactive"]);
  const retained = view("panels.panel.retained");
  publish([retained]);
  const registered = kernel.views.get(retained.id);
  let notices = 0;
  kernel.subscribe(() => notices++);
  const invalid = [
    view("panels.panel.foreign", "other"),
    view("foreign.panel.x"),
    view("panels.panel.invalid!"),
    view("panels.panel." + "x".repeat(121)),
    view("panels.panel.bad", "panels", { label: "  " }),
    view("panels.panel.bad", "panels", { icon: {} }),
    view("panels.panel.bad", "panels", { render: undefined }),
    view("panels.panel.bad", "panels", { settings: true }),
    view("panels.panel.bad", "panels", { tone: "red" }),
    view("panels.panel.static"),
    view("panels.panel.reserved"),
    null,
  ];
  for (const bad of invalid) {
    assert.throws(() => publish([view("panels.panel.candidate"), bad]));
    assert.deepEqual(ids(kernel), [retained.id, "panels.panel.static"].sort());
    assert.equal(kernel.views.get(retained.id), registered);
    assert.equal(notices, 0);
  }
  assert.throws(() => publish([retained, retained]), /Duplicate/);
  assert.throws(
    () => publish([0, 1, 2, 3].map((n) => view(`panels.panel.${n}`))),
    /quota/,
  );
  assert.throws(() => publish({}), /quota/);
  assert.equal(notices, 0);
  assert.equal(kernel.views.get(retained.id), registered);
});

await test("a live dynamic collision cannot replace another owner's registration", async (t) => {
  const publishers = {};
  const owners = ["root", "root.panel"];
  const kernel = own(
    t,
    owners.map((id) => ({
      manifest: manifest(id, {
        dynamicViews: { prefix: "root.panel.", limit: 3 },
      }),
      apply(_ctx, services) {
        publishers[id] = services.replaceViews;
      },
    })),
  );
  await kernel.configure([]);
  const shared = "root.panel.shared";
  publishers.root([view(shared, "root")]);
  const registered = kernel.views.get(shared);
  assert.throws(
    () => publishers["root.panel"]([view(shared, "root.panel")]),
    /Duplicate/,
  );
  assert.equal(kernel.views.get(shared), registered);
  await kernel.configure(["root"]);
  publishers["root.panel"]([view(shared, "root.panel")]);
  assert.equal(kernel.views.get(shared).owner, "root.panel");
});

await test("plugins without a dynamic declaration cannot publish", async (t) => {
  let publish;
  const kernel = own(t, [
    {
      manifest: manifest("panels", { dynamicViews: undefined }),
      apply(_ctx, services) {
        publish = services.replaceViews;
      },
    },
  ]);
  await kernel.configure([]);
  assert.throws(() => publish([view("panels.panel.x")]), /Undeclared/);
  assert.equal(kernel.views.size, 0);
});

await test("maximum publication count and 120 character IDs are accepted exactly", async (t) => {
  let publish;
  const kernel = own(t, [
    {
      manifest: manifest("panels", {
        dynamicViews: { prefix: "panels.panel.", limit: 384 },
      }),
      apply(_ctx, services) {
        publish = services.replaceViews;
      },
    },
  ]);
  await kernel.configure([]);
  const longest = "panels.panel." + "x".repeat(120 - "panels.panel.".length);
  const views = [
    view(longest),
    ...Array.from({ length: 383 }, (_, n) => view(`panels.panel.${n}`)),
  ];
  publish(views);
  assert.equal(kernel.views.size, 384);
  assert.equal(kernel.views.get(longest).id.length, 120);
  assert.throws(() => publish([...views, view("panels.panel.extra")]), /quota/);
  assert.equal(kernel.views.size, 384);
});

await test("static selector metadata survives lazy unload while dynamic entries retire", async (t) => {
  let publish;
  let creates = 0;
  const kernel = own(
    t,
    assemblePlugins(
      [manifest("panels", { views: ["panels.static"] })],
      [
        {
          id: "panels",
          create(manifest) {
            creates++;
            return {
              manifest,
              views: [
                view("panels.static", "panels", { label: "Static label" }),
              ],
              apply(_ctx, services) {
                publish = services.replaceViews;
              },
            };
          },
        },
      ],
    ),
  );
  assert.deepEqual(kernel.listViews(), [
    { id: "panels.static", owner: "panels", label: "panels.static" },
  ]);
  assert.equal(creates, 0);
  await kernel.configure([]);
  publish([view("panels.panel.x", "panels", { label: "Panel x" })]);
  assert.equal(kernel.listViews().length, 2);
  await kernel.configure(["panels"]);
  assert.equal(kernel.views.size, 0);
  assert.deepEqual(kernel.listViews(), [
    { id: "panels.static", owner: "panels", label: "Static label" },
  ]);
  assert.equal(kernel.plugins[0].views, undefined);
  kernel.listViews()[0].label = "Caller mutation";
  assert.equal(kernel.listViews()[0].label, "Static label");
});

await test("unload retires publishers before async cleanup and old cleanup cannot erase retry", async (t) => {
  const cleanupEntered = gate(),
    cleanupRelease = gate();
  const publishers = [],
    contexts = [];
  const kernel = own(t, [
    {
      manifest: manifest(),
      apply(ctx, { replaceViews }) {
        const round = publishers.length;
        publishers.push(replaceViews);
        contexts.push(ctx);
        replaceViews([
          view("panels.panel.saved", "panels", { label: `Round ${round}` }),
        ]);
        ctx.effect(() => async () => {
          if (round === 0) {
            cleanupEntered.resolve();
            await cleanupRelease.promise;
          }
        });
      },
    },
  ]);
  await kernel.configure([]);
  const retirement = kernel.configure(["panels"]);
  assert.throws(() => publishers[0]([]), /unavailable/);
  await cleanupEntered.promise;
  assert.equal(kernel.views.size, 0);
  assert.throws(
    () => publishers[0]([view("panels.panel.late")]),
    /unavailable/,
  );
  const reentry = kernel.configure([]);
  cleanupRelease.resolve();
  await Promise.all([retirement, reentry]);
  assert.equal(publishers.length, 2);
  const registered = kernel.views.get("panels.panel.saved");
  assert.equal(registered.label, "Round 1");
  assert.throws(() => publishers[0]([]), /unavailable/);
  await contexts[0].fiber.dispose();
  assert.equal(kernel.views.get("panels.panel.saved"), registered);
  publishers[1]([view("panels.panel.saved")]);
  assert.equal(kernel.views.size, 1);
});

await test("failed apply and failed render retire their publishers before retry", async (t) => {
  let fail = true;
  const publishers = [];
  const kernel = own(t, [
    {
      manifest: manifest(),
      apply(_ctx, { replaceViews }) {
        publishers.push(replaceViews);
        replaceViews([view("panels.panel.x")]);
        if (fail) throw new Error("module failure");
      },
    },
  ]);
  await kernel.configure([]);
  assert.equal(kernel.statuses.get("panels").state, "failed");
  assert.equal(kernel.views.size, 0);
  assert.throws(() => publishers[0]([view("panels.panel.x")]), /unavailable/);
  fail = false;
  await kernel.retry();
  assert.equal(kernel.views.size, 1);
  await kernel.reportFailure("panels", new Error("render failure"));
  assert.equal(kernel.views.size, 0);
  assert.throws(() => publishers[1]([]), /unavailable/);
  await kernel.retry();
  assert.equal(kernel.views.size, 1);
  assert.throws(() => publishers[0]([]), /unavailable/);
  assert.throws(() => publishers[1]([]), /unavailable/);
  publishers[2]([]);
  assert.equal(kernel.views.size, 0);
});

await test("direct Cordis disposal rejects publication during async plugin cleanup", async (t) => {
  const entered = gate(),
    release = gate();
  t.after(() => release.resolve());
  let context, publish;
  const kernel = own(t, [
    {
      manifest: manifest(),
      apply(ctx, { replaceViews }) {
        context = ctx;
        publish = replaceViews;
        publish([view("panels.panel.x")]);
        ctx.effect(() => async () => {
          entered.resolve();
          await release.promise;
        });
      },
    },
  ]);
  await kernel.configure([]);
  const disposing = context.fiber.dispose();
  await entered.promise;
  assert.throws(() => publish([view("panels.panel.late")]), /unavailable/);
  release.resolve();
  await disposing;
  assert.equal(kernel.views.size, 0);
  assert.throws(() => publish([]), /unavailable/);
});

for (const nextScopes of [["launcher"], ["rh-code"]]) {
  await test(`overlapping configure retires an unfinished publisher (${nextScopes})`, async (t) => {
    const entered = gate(),
      release = gate();
    const publishers = [],
      lateErrors = [];
    let rounds = 0;
    const kernel = own(t, [
      {
        manifest: manifest(),
        async apply(_ctx, { replaceViews }) {
          publishers.push(replaceViews);
          replaceViews([view("panels.panel.early")]);
          if (rounds++ === 0) {
            entered.resolve();
            await release.promise;
            try {
              replaceViews([view("panels.panel.late")]);
            } catch (error) {
              lateErrors.push(error);
            }
          }
        },
      },
    ]);
    const first = kernel.configure([]);
    await entered.promise;
    const second = kernel.configure([], nextScopes);
    assert.throws(() => publishers[0]([]), /unavailable/);
    release.resolve();
    await Promise.all([first, second]);
    assert.equal(lateErrors.length, 1);
    assert.match(lateErrors[0].message, /unavailable/);
    if (nextScopes[0] === "launcher") {
      assert.equal(kernel.views.size, 0);
      assert.equal(kernel.statuses.get("panels").state, "dormant");
      await kernel.configure([], ["rh-code"]);
    }
    assert.equal(kernel.statuses.get("panels").state, "active");
    assert.equal(publishers.length, 2);
    assert.deepEqual(ids(kernel), ["panels.panel.early"]);
    assert.throws(() => publishers[0]([]), /unavailable/);
    publishers[1]([]);
  });
}

await test("dispose rejects active and unfinished publishers immediately", async (t) => {
  for (const unfinished of [false, true]) {
    const entered = gate(),
      release = gate();
    let publish;
    const kernel = own(t, [
      {
        manifest: manifest(),
        async apply(_ctx, { replaceViews }) {
          publish = replaceViews;
          publish([view("panels.panel.x")]);
          entered.resolve();
          if (unfinished) await release.promise;
        },
      },
    ]);
    const activation = kernel.configure([]);
    await entered.promise;
    if (!unfinished) await activation;
    const stopping = kernel.dispose();
    assert.throws(() => publish([view("panels.panel.late")]), /unavailable/);
    release.resolve();
    await Promise.all([activation, stopping]);
    assert.equal(kernel.views.size, 0);
    await kernel.configure([]);
    assert.equal(kernel.statuses.get("panels").state, "stopped");
    assert.throws(() => publish([]), /unavailable/);
  }
});
