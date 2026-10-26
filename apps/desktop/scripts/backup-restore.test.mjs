import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";
import {
  assemblePlugins,
  snapshotEntry,
  own,
  createBackupRestoreConnection,
} from "./launcher-surface-fixture.mjs";
import { harness, elements, content, deferred } from "./maker-test-harness.mjs";
const manifest = JSON.parse(
  await readFile(
    new URL("../../../code/plugins.json", import.meta.url),
    "utf8",
  ),
).find((p) => p.id === snapshotEntry.id);
const button = (ui, label) => {
  const value = elements(
    ui.tree,
    (n) => n.type === "button" && content(n).includes(label),
  )[0];
  assert.ok(value, `Missing ${label}`);
  return value;
};
function fixture() {
  const calls = [],
    tasks = [];
  let current = true;
  const connection = createBackupRestoreConnection({
    isCurrent: () => current,
    onTask: (...args) => tasks.push(args),
    api: async (command, args) => {
      calls.push({ command, args });
      if (command === "instance_snapshot_pick")
        return {
          status: "selected",
          source: "/private/backup.zip",
          filename: "backup.zip",
          suggested_name: "Restored",
        };
      if (command === "instance_snapshot_restore_prepare")
        return {
          revision: "checked",
          name: args.name,
          pack_name: "Example",
          pack_version: "backup-1",
          minecraft: "1.20.1",
          bytes: 1024,
          file_count: 12,
          warnings: ["Runtime files remain shared"],
        };
      return { id: "restore-task" };
    },
  });
  return {
    connection,
    calls,
    tasks,
    retire: () => {
      current = false;
    },
  };
}
test("snapshot native service and both surfaces share the same plugin lifecycle", async (t) => {
  let activated = 0;
  const kernel = own(
    t,
    assemblePlugins([manifest], [snapshotEntry], {
      nativeServices: {
        activate: async (id) => {
          assert.equal(id, manifest.id);
          activated++;
        },
      },
    }),
  );
  await kernel.configure([manifest.id], ["launcher"]);
  assert.equal(activated, 0);
  await kernel.configure([], ["launcher"]);
  assert.equal(activated, 1);
  const surface = kernel.listLauncherSurfaces("instance.overview.tools")[0];
  const children = elements(
    surface.render({ host: {}, lifetime: surface.lifetime }),
    (n) => ["SnapshotSurface", "BackupRestoreSurface"].includes(n.type?.name),
  );
  assert.equal(children.length, 2);
  await kernel.configure([manifest.id], ["launcher"]);
  assert.equal(surface.lifetime.isActive(), false);
});
test("selections and previews hide paths, reject copies and submit one checked target", async () => {
  const f = fixture();
  const selection = await f.connection.pick();
  assert.deepEqual(Object.keys(selection).sort(), [
    "filename",
    "suggestedName",
  ]);
  assert.ok(Object.isFrozen(selection));
  await assert.rejects(
    f.connection.prepare({ ...selection }, "Restored"),
    /expired/,
  );
  const preview = await f.connection.prepare(selection, "Restored");
  assert.equal(preview.source, undefined);
  assert.equal(preview.revision, undefined);
  assert.ok(Object.isFrozen(preview.warnings));
  await assert.rejects(
    f.connection.start({ ...preview }, () => true),
    /again/,
  );
  await assert.rejects(
    f.connection.start(preview, () => false),
    /retired/,
  );
  await f.connection.start(preview, () => true);
  assert.deepEqual(f.calls[2], {
    command: "instance_snapshot_restore_start",
    args: {
      source: "/private/backup.zip",
      name: "Restored",
      revision: "checked",
    },
  });
  await assert.rejects(
    f.connection.start(preview, () => true),
    /again/,
  );
  assert.deepEqual(f.tasks, [["restore-task", true]]);
});
test("late accepted restores stay tracked after target and plugin retirement", async () => {
  const pending = deferred(),
    tasks = [];
  let active = true;
  const f = fixture();
  const selected = await f.connection.pick();
  const preview = await f.connection.prepare(selected, "Restored");
  f.retire();
  await assert.rejects(f.connection.prepare(selected, "Restored"), /retired/);
  await assert.rejects(
    f.connection.start(preview, () => true),
    /retired/,
  );
  const connection = createBackupRestoreConnection({
    isCurrent: () => active,
    onTask: (...args) => tasks.push(args),
    api: async (command, args) =>
      command === "instance_snapshot_pick"
        ? {
            status: "selected",
            source: "/backup.zip",
            filename: "backup.zip",
            suggested_name: "Restored",
          }
        : command === "instance_snapshot_restore_prepare"
          ? { revision: "rev", name: args.name, warnings: [] }
          : pending.promise,
  });
  const choice = await connection.pick();
  const checked = await connection.prepare(choice, "Restored");
  const saving = connection.start(checked, () => active);
  active = false;
  pending.resolve({ id: "accepted" });
  await saving;
  assert.deepEqual(tasks, [["accepted", false]]);
});
async function formFixture(t) {
  const kernel = own(t, assemblePlugins([manifest], [snapshotEntry]));
  await kernel.configure([], ["launcher"]);
  const f = fixture();
  let config = {
    restore: f.connection,
    available: true,
    localization: { language: "en-US", region: "US" },
  };
  const ui = harness(
    "BackupRestoreSurface",
    {
      host: { instanceSnapshots: config },
      lifetime: kernel.listLauncherSurfaces("instance.overview.tools")[0]
        .lifetime,
    },
    { module: "plugins/instances/BackupRestoreSurface.tsx" },
  );
  t.after(() => ui.close());
  return {
    ...f,
    ui,
    kernel,
    update(patch) {
      config = { ...config, ...patch };
      ui.replace({ host: { instanceSnapshots: config } });
    },
  };
}
test("name edits and choosing another backup retire reviewed plans and saved callbacks", async (t) => {
  const f = await formFixture(t);
  button(f.ui, "Select backup").props.onClick();
  await f.ui.flush();
  button(f.ui, "Inspect backup").props.onClick();
  await f.ui.flush();
  const stale = button(f.ui, "Restore as new").props.onClick;
  elements(f.ui.tree, (n) => n.type === "input")[0].props.onChange({
    target: { value: "Another" },
  });
  await f.ui.flush();
  stale();
  await f.ui.flush();
  assert.equal(f.calls.length, 2);
  assert.doesNotMatch(content(f.ui.tree), /Restore as new instance/);
  button(f.ui, "Inspect backup").props.onClick();
  await f.ui.flush();
  assert.equal(f.calls[2].args.name, "Another");
  const old = button(f.ui, "Restore as new").props.onClick;
  button(f.ui, "Select backup").props.onClick();
  await f.ui.flush();
  old();
  await f.ui.flush();
  assert.equal(f.calls.length, 4);
  assert.doesNotMatch(content(f.ui.tree), /12 files/);
});
test("double checks are blocked; late responses from the old host do not repaint", async (t) => {
  const f = await formFixture(t),
    old = deferred();
  button(f.ui, "Select backup").props.onClick();
  await f.ui.flush();
  let checks = 0;
  f.update({
    restore: {
      ...f.connection,
      prepare: () => {
        checks++;
        return old.promise;
      },
    },
  });
  // Replacing the bound connection clears the old selection.
  button(f.ui, "Select backup").props.onClick();
  await f.ui.flush();
  const click = button(f.ui, "Inspect backup").props.onClick;
  click();
  click();
  await f.ui.flush();
  assert.equal(checks, 1);
  f.update({ restore: f.connection });
  old.resolve({ name: "OLD", fileCount: 999, bytes: 999, warnings: [] });
  await f.ui.flush();
  assert.doesNotMatch(content(f.ui.tree), /999|OLD/);
  const saved = button(f.ui, "Select backup").props.onClick;
  await f.kernel.configure([manifest.id], ["launcher"]);
  const count = f.calls.length;
  saved();
  await f.ui.flush();
  assert.equal(f.calls.length, count);
});
test("cancelled chooser keeps the current backup and its still-valid preview", async (t) => {
  const f = await formFixture(t);
  let picks = 0;
  f.update({
    restore: {
      ...f.connection,
      pick: () => (++picks === 1 ? f.connection.pick() : Promise.resolve(null)),
    },
  });
  button(f.ui, "Select backup").props.onClick();
  await f.ui.flush();
  button(f.ui, "Inspect backup").props.onClick();
  await f.ui.flush();
  button(f.ui, "Select backup").props.onClick();
  await f.ui.flush();
  assert.match(content(f.ui.tree), /selection cancelled/);
  assert.match(content(f.ui.tree), /12 files/);
  button(f.ui, "Restore as new").props.onClick();
  await f.ui.flush();
  assert.deepEqual(f.tasks, [["restore-task", true]]);
});
test("actual host retires backup selections with root, revision, availability and unmount", async (t) => {
  const pending = deferred(),
    calls = [];
  const ui = harness(
    "useInstanceSnapshotHost",
    {
      host: {
        announcements: { localization: { language: "en-US", region: "US" } },
      },
      api: async (command) => {
        calls.push(command);
        return pending.promise;
      },
      id: "Source",
      context: "root-a:rev-1",
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
  const old = ui.tree.instanceSnapshots.restore;
  const picking = old.pick();
  ui.replace({ id: "Other", context: "root-b:rev-1" });
  pending.resolve({
    status: "selected",
    source: "/old.zip",
    filename: "old.zip",
    suggested_name: "Old",
  });
  await assert.rejects(picking, /retired/);
  const changed = ui.tree.instanceSnapshots.restore;
  ui.replace({ context: "root-b:rev-2" });
  await assert.rejects(changed.pick(), /retired/);
  const available = ui.tree.instanceSnapshots.restore;
  ui.replace({ available: false });
  ui.replace({ available: true });
  await assert.rejects(available.pick(), /retired/);
  const mounted = ui.tree.instanceSnapshots.restore;
  ui.close();
  await assert.rejects(mounted.pick(), /retired/);
  assert.equal(calls.length, 1);
});
