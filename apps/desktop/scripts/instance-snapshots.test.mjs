/** Actual bundled plugin, instance-scoped port and form handlers. Files, native
 * dialogs and tasks are deterministic fixtures; no user game data is exported.
 */
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import {
  assemblePlugins,
  own,
  createLauncherSurfacePort,
  snapshotEntry,
  createInstanceSnapshotConnection,
  snapshotRequest,
} from "./launcher-surface-fixture.mjs";
import { harness, elements, content, deferred } from "./maker-test-harness.mjs";

const manifest = JSON.parse(
  await readFile(new URL("../../../code/plugins.json", import.meta.url)),
).find((p) => p.id === snapshotEntry.id);
const button = (ui, text) => {
  const found = elements(
    ui.tree,
    (n) => n.type === "button" && content(n).includes(text),
  )[0];
  assert.ok(found, `Missing button ${text}`);
  return found;
};
const plan = (request) => ({
  revision: "checked",
  instance_id: "Example",
  request,
  file_count: 12,
  bytes: 4096,
  warnings: ["Only declared content is included"],
});
function connectionFixture() {
  let current = true;
  const calls = [],
    tasks = [];
  const api = async (command, args) => {
    calls.push({ command, args });
    return command === "instance_export_plan"
      ? plan(args.request)
      : "task-example";
  };
  const connection = createInstanceSnapshotConnection({
    api,
    id: "Example",
    isCurrent: () => current,
    onTask: (...args) => tasks.push(args),
  });
  return {
    connection,
    api,
    calls,
    tasks,
    retire: () => {
      current = false;
    },
  };
}

test("real snapshot entry is lazy, optional, scope independent and revocable", async (t) => {
  let imports = 0;
  const kernel = own(
    t,
    assemblePlugins([manifest], {
      [manifest.id]: [
        async () => {
          imports++;
          return snapshotEntry;
        },
      ],
    }),
  );
  await kernel.configure([manifest.id], ["launcher"]);
  assert.equal(imports, 0);
  await kernel.configure([], ["launcher"]);
  const [surface] = kernel.listLauncherSurfaces("instance.overview.tools");
  assert.equal(surface.owner, manifest.id);
  await kernel.configure([], ["launcher", "rh-code"]);
  await kernel.configure([], ["launcher"]);
  assert.equal(imports, 1);
  assert.equal(surface.lifetime.isActive(), true);
  await kernel.configure([manifest.id], ["launcher"]);
  assert.equal(surface.lifetime.isActive(), false);
  assert.deepEqual(kernel.listLauncherSurfaces("instance.overview.tools"), []);
  const invalid = own(
    t,
    assemblePlugins([{ ...manifest, capabilities: [] }], {
      [manifest.id]: [
        async () => {
          throw Error("must not import");
        },
      ],
    }),
  );
  await invalid.configure([], ["launcher"]);
  assert.equal(invalid.statuses.get(manifest.id).state, "failed");
});

test("backup preserves worlds and personal settings; sharing excludes those exact groups", () => {
  const now = new Date("2026-10-25T04:00:00Z");
  const backup = snapshotRequest("Example", "backup", now);
  const share = snapshotRequest("Example", "share", now);
  for (const key of [
    "game",
    "mods",
    "packData",
    "modSettings",
    "resourcepacks",
    "shaderpacks",
  ])
    assert.equal(backup.checks[key] && share.checks[key], true, key);
  for (const key of [
    "saves",
    "server",
    "gameSettings",
    "gamePersonal",
    "maps",
    "jeiPersonal",
    "guidePersonal",
  ])
    assert.deepEqual(
      [backup.checks[key], share.checks[key]],
      [true, false],
      key,
    );
  for (const key of [
    "screenshots",
    "other",
    "launcher",
    "bundleAssets",
    "modrinth",
  ])
    assert.deepEqual(
      [backup.checks[key], share.checks[key]],
      [false, false],
      key,
    );
  assert.match(backup.version, /^backup-20261025T040000000Z$/);
  assert.throws(() => snapshotRequest("Example", "shell"), /Invalid snapshot/);
});

test("preview carries no writable submission and is single use", async () => {
  const f = connectionFixture();
  const preview = await f.connection.prepare("backup");
  assert.deepEqual(Object.keys(preview).sort(), [
    "bytes",
    "fileCount",
    "warnings",
  ]);
  assert.ok(Object.isFrozen(preview.warnings));
  f.calls[0].args.request.checks.saves = false;
  await assert.rejects(
    f.connection.start({ ...preview }, () => true),
    /Inspect.*again/,
  );
  await f.connection.start(preview, () => true);
  assert.equal(f.calls[1].args.plan.instance_id, "Example");
  assert.equal(f.calls[1].args.plan.request.checks.saves, true);
  assert.deepEqual(f.tasks, [["task-example", true]]);
  await assert.rejects(
    f.connection.start(preview, () => true),
    /Inspect.*again/,
  );
});

test("retired targets and retired plugins cannot submit an old preview", async () => {
  const f = connectionFixture();
  const preview = await f.connection.prepare("backup");
  await assert.rejects(
    f.connection.start(preview, () => false),
    /plugin retired/,
  );
  f.retire();
  await assert.rejects(f.connection.prepare("share"), /context retired/);
  await assert.rejects(
    f.connection.start(preview, () => true),
    /context retired/,
  );
  assert.equal(f.calls.length, 1);
});

test("an admitted save remains tracked after plugin retirement without navigating", async () => {
  const pending = deferred(),
    tasks = [];
  let active = true;
  const connection = createInstanceSnapshotConnection({
    id: "Example",
    isCurrent: () => true,
    onTask: (...args) => tasks.push(args),
    api: async (command, args) =>
      command === "instance_export_plan" ? plan(args.request) : pending.promise,
  });
  const preview = await connection.prepare("backup");
  const saving = connection.start(preview, () => active);
  active = false;
  pending.resolve("accepted-task");
  assert.equal(await saving, "accepted-task");
  assert.deepEqual(tasks, [["accepted-task", false]]);
});

async function formFixture(t) {
  const kernel = own(t, assemblePlugins([manifest], [snapshotEntry]));
  await kernel.configure([], ["launcher"]);
  const port = createLauncherSurfacePort();
  t.after(port.attach(kernel));
  const fixture = connectionFixture();
  let config = {
    connection: fixture.connection,
    available: true,
    localization: { language: "en-US", region: "US" },
  };
  const ui = harness(
    "SnapshotSurface",
    {
      host: { instanceSnapshots: config },
      lifetime: port.list("instance.overview.tools")[0].lifetime,
    },
    { module: "plugins/instances/SnapshotSurface.tsx" },
  );
  t.after(() => ui.close());
  return {
    ...fixture,
    ui,
    kernel,
    update(patch) {
      config = { ...config, ...patch };
      ui.replace({ host: { instanceSnapshots: config } });
    },
  };
}

test("form invalidates a reviewed backup and saved handlers when the preset changes", async (t) => {
  const f = await formFixture(t);
  button(f.ui, "Inspect backup").props.onClick();
  await f.ui.flush();
  const staleSave = button(f.ui, "Save ZIP").props.onClick;
  const select = elements(f.ui.tree, (n) => n.type?.name === "CeSelect")[0];
  select.props.onChange({ target: { value: "share" } });
  await f.ui.flush();
  assert.equal(
    elements(
      f.ui.tree,
      (n) => n.type === "button" && content(n).includes("Save ZIP"),
    ).length,
    0,
  );
  staleSave();
  await f.ui.flush();
  assert.equal(f.calls.length, 1);
  button(f.ui, "Inspect backup").props.onClick();
  await f.ui.flush();
  assert.equal(f.calls[1].args.request.checks.saves, false);
});

test("form rejects double submissions, hides late old-instance results and can prepare the next instance", async (t) => {
  const f = await formFixture(t),
    first = deferred();
  const calls = [];
  f.update({
    connection: {
      prepare: () => {
        calls.push("old");
        return first.promise;
      },
    },
  });
  const click = button(f.ui, "Inspect backup").props.onClick;
  click();
  click();
  await f.ui.flush();
  assert.deepEqual(calls, ["old"]);
  f.update({ connection: f.connection });
  button(f.ui, "Inspect backup").props.onClick();
  await f.ui.flush();
  first.resolve({ fileCount: 999, bytes: 123, warnings: ["OLD INSTANCE"] });
  await f.ui.flush();
  assert.doesNotMatch(content(f.ui.tree), /OLD INSTANCE|999/);
  assert.match(content(f.ui.tree), /12 files/);
});

test("retirement revokes saved form actions; cancelled save keeps no preview", async (t) => {
  const f = await formFixture(t);
  f.update({ connection: { ...f.connection, start: async () => null } });
  button(f.ui, "Inspect backup").props.onClick();
  await f.ui.flush();
  button(f.ui, "Save ZIP").props.onClick();
  await f.ui.flush();
  assert.match(content(f.ui.tree), /cancel/i);
  const staleInspect = button(f.ui, "Inspect backup").props.onClick;
  const count = f.calls.length;
  await f.kernel.configure([manifest.id], ["launcher"]);
  staleInspect();
  await f.ui.flush();
  assert.equal(f.calls.length, count);
});

test("the actual host port retires API, revision, availability and unmount ownership", async (t) => {
  const first = deferred(),
    calls = [];
  const api = async (command, args) => {
    calls.push(args.id);
    if (calls.length === 1) return first.promise;
    return { ...plan(args.request), instance_id: args.id };
  };
  const ui = harness(
    "useInstanceSnapshotHost",
    {
      host: {
        announcements: { localization: { language: "en-US", region: "US" } },
      },
      api,
      id: "Example",
      context: "root-a:revision-1",
      available: true,
      onTask() {},
    },
    {
      module: "productPlugins/useInstanceSnapshotHost.ts",
      render: (m, p) =>
        m.useInstanceSnapshotHost(
          p.host,
          p.api,
          p.id,
          p.context,
          p.available,
          p.onTask,
        ),
    },
  );
  t.after(() => ui.close());
  const old = ui.tree.instanceSnapshots.connection;
  const late = old.prepare("backup");
  ui.replace({ id: "Other", context: "root-b:revision-1" });
  first.resolve(plan(snapshotRequest("Example", "backup")));
  await assert.rejects(late, /context retired/);
  const current = ui.tree.instanceSnapshots.connection;
  await current.prepare("share");
  assert.deepEqual(calls, ["Example", "Other"]);
  ui.replace({ context: "root-b:revision-2" });
  await assert.rejects(current.prepare("backup"), /context retired/);
  const beforeBusy = ui.tree.instanceSnapshots.connection;
  ui.replace({ available: false });
  ui.replace({ available: true });
  await assert.rejects(beforeBusy.prepare("backup"), /context retired/);
  const mounted = ui.tree.instanceSnapshots.connection;
  ui.close();
  await assert.rejects(mounted.prepare("backup"), /context retired/);
  assert.equal(calls.length, 2);
});
