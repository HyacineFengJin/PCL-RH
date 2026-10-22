import assert from "node:assert/strict";
import { test } from "node:test";
import {
  assemblePlugins,
  createLauncherSurfacePort,
  LauncherSurfaceBoundary,
  manifest,
  plugin,
  records,
  own,
  gate,
} from "./launcher-surface-fixture.mjs";

test("launcher declarations are checked before importing an entry", async (t) => {
  let imports = 0;
  for (const extra of [
    { launcherSurfaces: null },
    { launcherSurfaces: {} },
    { launcherSurfaces: [null] },
    { launcherSurfaces: [{ id: "feed.component", slot: "foreign" }] },
    {
      launcherSurfaces: [
        { id: "feed.component", slot: "home.announcements", extra: true },
      ],
    },
    {
      launcherSurfaces: [
        { id: "feed.component", slot: "home.announcements" },
        { id: "feed.component", slot: "home.announcements" },
      ],
    },
    { capabilities: [] },
    { scope: "rh-code" },
  ]) {
    const m = manifest("feed", extra);
    const kernel = own(
      t,
      assemblePlugins([m], {
        feed: [
          async () => {
            imports++;
            return { id: "feed", create: (manifest) => ({ manifest }) };
          },
        ],
      }),
    );
    await kernel.configure([], ["launcher"]);
    assert.equal(kernel.statuses.get("feed").state, "failed");
    assert.equal(records(kernel).length, 0);
  }
  assert.equal(imports, 0);
});

test("missing, foreign and invalid component contributions fail their owner", async (t) => {
  for (const contribution of [
    undefined,
    [],
    [
      {
        id: "feed.component",
        owner: "other",
        slot: "home.announcements",
        render: () => null,
      },
    ],
    [
      {
        id: "feed.component",
        owner: "feed",
        slot: "home.announcements",
        render: {},
      },
    ],
  ]) {
    const kernel = own(t, [plugin("feed", { launcherSurfaces: contribution })]);
    await kernel.configure([], ["launcher"]);
    assert.equal(kernel.statuses.get("feed").state, "failed");
    assert.equal(records(kernel).length, 0);
  }
});

test("components publish only after successful activation and keep identity across Code scopes", async (t) => {
  const entered = gate(),
    release = gate();
  t.after(() => release.resolve());
  const kernel = own(t, [
    plugin("feed", {
      async apply() {
        entered.resolve();
        await release.promise;
      },
    }),
  ]);
  const pending = kernel.configure([], ["launcher"]);
  await entered.promise;
  assert.deepEqual(records(kernel), []);
  release.resolve();
  await pending;
  const [record] = records(kernel);
  assert.ok(Object.isFrozen(record));
  await kernel.configure([], ["launcher", "rh-code"]);
  await kernel.configure([], ["launcher"]);
  assert.equal(records(kernel)[0], record);
});

test("superseded disable still revokes old handlers and creates a new activation", async (t) => {
  const kernel = own(t, [plugin()]);
  await kernel.configure([], ["launcher"]);
  const [old] = records(kernel);
  const off = kernel.configure(["feed"], ["launcher"]);
  assert.equal(old.lifetime.isActive(), false);
  assert.deepEqual(records(kernel), []);
  const on = kernel.configure([], ["launcher"]);
  await Promise.all([off, on]);
  assert.notEqual(records(kernel)[0].generation, old.generation);
  assert.equal(old.lifetime.isActive(), false);
  assert.equal(records(kernel)[0].lifetime.isActive(), true);
});

test("host revocation precedes native waits and cannot reactivate saved actions", async (t) => {
  const kernel = own(t, [plugin()]);
  await kernel.configure([], ["launcher"]);
  const [old] = records(kernel);
  kernel.revokeLauncherSurfaces(["feed"]);
  assert.equal(old.lifetime.isActive(), false);
  assert.deepEqual(records(kernel), []);
  assert.equal(kernel.statuses.get("feed").state, "active");
  await kernel.configure([], ["launcher"]);
  assert.equal(records(kernel)[0].lifetime.isActive(), true);
  assert.notEqual(records(kernel)[0].generation, old.generation);
  assert.equal(old.lifetime.isActive(), false);
});

test("dependency retirement revokes dependents immediately without affecting siblings", async (t) => {
  const kernel = own(t, [
    plugin("parent"),
    plugin("child", {
      manifest: manifest("child", { dependencies: ["parent"] }),
    }),
    plugin("sibling"),
  ]);
  await kernel.configure([], ["launcher"]);
  const before = records(kernel);
  const off = kernel.configure(["parent"], ["launcher"]);
  assert.deepEqual(
    records(kernel).map((r) => r.owner),
    ["sibling"],
  );
  assert.equal(
    before.find((r) => r.owner === "child").lifetime.isActive(),
    false,
  );
  await off;
  assert.equal(kernel.statuses.get("child").state, "blocked");
});

test("direct Cordis disposal revokes a component before asynchronous cleanup", async (t) => {
  const entered = gate(),
    release = gate();
  let context;
  t.after(() => release.resolve());
  const kernel = own(t, [
    plugin("feed", {
      apply(ctx) {
        context = ctx;
        ctx.effect(() => async () => {
          entered.resolve();
          await release.promise;
        });
      },
    }),
  ]);
  await kernel.configure([], ["launcher"]);
  const [record] = records(kernel);
  const disposing = context.fiber.dispose();
  await entered.promise;
  assert.equal(record.lifetime.isActive(), false);
  assert.deepEqual(records(kernel), []);
  release.resolve();
  await disposing;
});

test("port detachment and late cleanup cannot revive or retire another binding", async (t) => {
  const kernel = own(t, [plugin()]);
  await kernel.configure([], ["launcher"]);
  const port = createLauncherSurfacePort();
  const first = port.attach(kernel);
  const [old] = port.list("home.announcements");
  const second = port.attach(kernel);
  const [current] = port.list("home.announcements");
  first();
  assert.equal(old.lifetime.isActive(), false);
  assert.equal(current.lifetime.isActive(), true);
  assert.notEqual(old.generation, current.generation);
  await port.reportFailure(old, new Error("late failure"));
  assert.equal(kernel.statuses.get("feed").state, "active");
  second();
  assert.equal(current.lifetime.isActive(), false);
  assert.deepEqual(port.effects(), []);
  assert.equal(records(kernel)[0].lifetime.isActive(), true);
});

test("a delayed boundary cannot fail a newer registration with the same owner ID", async (t) => {
  const kernel = own(t, [plugin(), plugin("sibling")]);
  await kernel.configure([], ["launcher"]);
  const port = createLauncherSurfacePort();
  t.after(port.attach(kernel));
  const [old] = port.list("home.announcements");
  const boundary = new LauncherSurfaceBoundary({
    port,
    surface: old,
    children: null,
  });
  await kernel.configure(["feed"], ["launcher"]);
  await kernel.configure([], ["launcher"]);
  boundary.componentDidCatch(new Error("retired render"));
  await kernel.retry();
  assert.equal(kernel.statuses.get("feed").state, "active");
  const current = port
    .list("home.announcements")
    .find((r) => r.owner === "feed");
  await port.reportFailure({ ...current }, new Error("copied descriptor"));
  assert.equal(kernel.statuses.get("feed").state, "active");
  const failure = port.reportFailure(current, new Error("current render"));
  assert.equal(current.lifetime.isActive(), false);
  await failure;
  assert.equal(kernel.statuses.get("feed").state, "failed");
  assert.equal(kernel.statuses.get("sibling").state, "active");
  await kernel.retry();
  assert.equal(current.lifetime.isActive(), false);
  assert.equal(port.list("home.announcements").length, 2);
});

test("disposing an activation still applying never publishes its component", async (t) => {
  const entered = gate(),
    release = gate();
  t.after(() => release.resolve());
  const kernel = own(t, [
    plugin("feed", {
      async apply() {
        entered.resolve();
        await release.promise;
      },
    }),
  ]);
  const activation = kernel.configure([], ["launcher"]);
  await entered.promise;
  const disposing = kernel.dispose();
  assert.deepEqual(records(kernel), []);
  release.resolve();
  await Promise.all([activation, disposing]);
  assert.deepEqual(records(kernel), []);
});

test("catalog snapshots launcher placement authority without evaluating components", () => {
  const original = manifest();
  const [definition] = assemblePlugins([original], {});
  assert.ok(Object.isFrozen(definition.manifest.launcherSurfaces));
  assert.ok(Object.isFrozen(definition.manifest.launcherSurfaces[0]));
  original.launcherSurfaces[0].slot = "foreign";
  assert.equal(
    definition.manifest.launcherSurfaces[0].slot,
    "home.announcements",
  );
});
