import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";
import {
  assemblePlugins,
  createInstanceCopyConnection,
  copyEntry,
  own,
  gate,
} from "./launcher-surface-fixture.mjs";
import { harness, elements, content } from "./maker-test-harness.mjs";
function button(ui, label) {
  const value = elements(
    ui.tree,
    (node) => node.type === "button" && content(node).includes(label),
  )[0];
  assert.ok(value, `Missing button ${label}`);
  return value;
}
const manifest = JSON.parse(
  await readFile(
    new URL("../../../code/plugins.json", import.meta.url),
    "utf8",
  ),
).find((m) => m.id === copyEntry.id);
const options = { name: "Copy", saves: false, screenshots: false };
function fixture() {
  const calls = [],
    tasks = [];
  let current = true;
  const connection = createInstanceCopyConnection({
    id: "Source",
    isCurrent: () => current,
    onTask: (...args) => tasks.push(args),
    api: async (command, args) => {
      calls.push({ command, args });
      return command === "instance_copy_prepare"
        ? {
            revision: "rev",
            instance_id: args.id,
            name: args.request.name,
            bytes: 1024,
            file_count: 12,
          }
        : { id: "accepted" };
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
test("official copy component is lazy, capability checked and independently retired", async (t) => {
  let imports = 0,
    native = 0;
  const kernel = own(
    t,
    assemblePlugins(
      [manifest],
      {
        [manifest.id]: [
          async () => {
            imports++;
            return copyEntry;
          },
        ],
      },
      {
        nativeServices: {
          activate: async () => {
            native++;
          },
        },
      },
    ),
  );
  await kernel.configure([manifest.id], ["launcher"]);
  assert.equal(imports, 0);
  await kernel.configure([], ["launcher"]);
  assert.equal(imports, 1);
  assert.equal(native, 1);
  const surface = kernel.listLauncherSurfaces("instance.overview.copy")[0];
  assert.equal(surface.owner, manifest.id);
  await kernel.configure([], ["launcher", "rh-code"]);
  await kernel.configure([], ["launcher"]);
  assert.equal(surface.lifetime.isActive(), true);
  await kernel.configure([manifest.id], ["launcher"]);
  assert.equal(surface.lifetime.isActive(), false);
  assert.deepEqual(kernel.listLauncherSurfaces("instance.overview.copy"), []);
  const invalid = own(
    t,
    assemblePlugins([{ ...manifest, capabilities: [] }], [copyEntry]),
  );
  await invalid.configure([], ["launcher"]);
  assert.equal(invalid.statuses.get(manifest.id).state, "failed");
});
test("native submission is private, target bound and single use", async () => {
  const f = fixture();
  const preview = await f.connection.prepare(options);
  assert.deepEqual(Object.keys(preview).sort(), ["bytes", "fileCount", "name"]);
  assert.ok(Object.isFrozen(preview));
  await assert.rejects(
    f.connection.start({ ...preview }, () => true),
    /expired/,
  );
  await assert.rejects(
    f.connection.start(preview, () => false),
    /retired/,
  );
  await f.connection.start(preview, () => true);
  assert.deepEqual(f.calls[1], {
    command: "instance_copy_start",
    args: { id: "Source", request: options, revision: "rev" },
  });
  assert.deepEqual(f.tasks, [["accepted", true]]);
  await assert.rejects(
    f.connection.start(preview, () => true),
    /expired/,
  );
});
test("retired targets cannot prepare or submit; accepted late tasks stay tracked", async () => {
  const f = fixture();
  const preview = await f.connection.prepare(options);
  f.retire();
  await assert.rejects(f.connection.prepare(options), /retired/);
  await assert.rejects(
    f.connection.start(preview, () => true),
    /retired/,
  );
  const pending = gate(),
    tasks = [];
  let active = true;
  const connection = createInstanceCopyConnection({
    id: "Source",
    isCurrent: () => active,
    onTask: (...args) => tasks.push(args),
    api: async (command) =>
      command === "instance_copy_prepare"
        ? {
            revision: "rev",
            instance_id: "Source",
            name: "Copy",
            bytes: 1,
            file_count: 1,
          }
        : pending.promise,
  });
  const checked = await connection.prepare(options);
  const saving = connection.start(checked, () => active);
  active = false;
  pending.resolve({ id: "late" });
  await saving;
  assert.deepEqual(tasks, [["late", false]]);
});
async function formFixture(t) {
  const kernel = own(
    t,
    assemblePlugins([manifest], [copyEntry], {
      nativeServices: { activate: async () => {} },
    }),
  );
  await kernel.configure([], ["launcher"]);
  const f = fixture();
  let config = {
    connection: f.connection,
    available: true,
    suggestedName: "Copy",
    localization: { language: "en-US", region: "US" },
  };
  const ui = harness(
    "CopySurface",
    {
      host: { instanceCopy: config },
      lifetime: kernel.listLauncherSurfaces("instance.overview.copy")[0]
        .lifetime,
    },
    { module: "plugins/instances/CopySurface.tsx" },
  );
  t.after(() => ui.close());
  return {
    ...f,
    ui,
    kernel,
    update(patch) {
      config = { ...config, ...patch };
      ui.replace({ host: { instanceCopy: config } });
    },
  };
}
test("editing name or world selection invalidates the preview and saved submit handlers", async (t) => {
  const f = await formFixture(t);
  button(f.ui, "Inspect copy").props.onClick();
  await f.ui.flush();
  const oldStart = button(f.ui, "Create copy").props.onClick;
  elements(
    f.ui.tree,
    (n) => n.type === "input" && n.props.type === "checkbox",
  )[0].props.onChange({ target: { checked: true } });
  await f.ui.flush();
  oldStart();
  await f.ui.flush();
  assert.equal(f.calls.length, 1);
  assert.doesNotMatch(content(f.ui.tree), /Create copy/);
  button(f.ui, "Inspect copy").props.onClick();
  await f.ui.flush();
  assert.equal(f.calls[1].args.request.saves, true);
  elements(
    f.ui.tree,
    (n) => n.type === "input" && !n.props.type,
  )[0].props.onChange({ target: { value: "Different" } });
  await f.ui.flush();
  assert.doesNotMatch(content(f.ui.tree), /Create copy/);
});
test("double clicks are blocked and late old-instance results never replace the new target", async (t) => {
  const f = await formFixture(t),
    old = gate(),
    calls = [];
  f.update({
    connection: {
      prepare: () => {
        calls.push("old");
        return old.promise;
      },
    },
  });
  const click = button(f.ui, "Inspect copy").props.onClick;
  click();
  click();
  await f.ui.flush();
  assert.deepEqual(calls, ["old"]);
  f.update({ connection: f.connection, suggestedName: "Copy" });
  button(f.ui, "Inspect copy").props.onClick();
  await f.ui.flush();
  old.resolve({ name: "OLD", bytes: 999, fileCount: 999 });
  await f.ui.flush();
  assert.doesNotMatch(content(f.ui.tree), /999/);
  assert.match(content(f.ui.tree), /12 files/);
  const stale = button(f.ui, "Create copy").props.onClick;
  await f.kernel.configure([manifest.id], ["launcher"]);
  stale();
  await f.ui.flush();
  assert.equal(f.calls.length, 1);
});
test("host revision, root, availability and unmount revoke actual copy connections", async (t) => {
  const pending = gate(),
    calls = [];
  const api = async (command, args) => {
    calls.push(args.id);
    return calls.length === 1
      ? pending.promise
      : {
          revision: "rev",
          instance_id: args.id,
          name: args.request.name,
          bytes: 1,
          file_count: 1,
        };
  };
  const ui = harness(
    "useInstanceSnapshotHost",
    {
      host: {
        announcements: { localization: { language: "en-US", region: "US" } },
      },
      api,
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
  const old = ui.tree.instanceCopy.connection;
  const late = old.prepare(options);
  ui.replace({ id: "Other", context: "root-b:rev-1" });
  pending.resolve({
    revision: "rev",
    instance_id: "Source",
    name: "Copy",
    bytes: 1,
    file_count: 1,
  });
  await assert.rejects(late, /retired/);
  const next = ui.tree.instanceCopy.connection;
  await next.prepare(options);
  ui.replace({ context: "root-b:rev-2" });
  await assert.rejects(next.prepare(options), /retired/);
  const available = ui.tree.instanceCopy.connection;
  ui.replace({ available: false });
  ui.replace({ available: true });
  await assert.rejects(available.prepare(options), /retired/);
  const mounted = ui.tree.instanceCopy.connection;
  ui.close();
  await assert.rejects(mounted.prepare(options), /retired/);
  assert.deepEqual(calls, ["Source", "Other"]);
});

test("copy tasks refresh global recovery projections and render copy steps in English", (t) => {
  for (const stage of ["complete", "error"]) {
    const ui = harness(
      "taskNeedsBootstrap",
      { task: { kind: "instance_copy", root_id: "root-a", stage } },
      {
        module: "taskLifecycle.ts",
        render: (m, p) => m.taskNeedsBootstrap(p.task, "root-b"),
      },
    );
    assert.equal(ui.tree, true);
    ui.close();
  }
  const status = {
    kind: "instance_copy",
    task_id: "copy-task",
    root_id: "root-a",
    version: "Copy",
    stage: "processing",
    phase: "import-extract",
    progress: 0.5,
    can_cancel: true,
    steps: [
      {
        id: "import-extract",
        label: "untranslated backend label",
        state: "running",
      },
    ],
  };
  const ui = harness(
    "TaskManager",
    {
      status,
      native: true,
      api: async () => ({}),
      onNotify() {},
      onStatusChange() {},
      onCancelled() {},
      onDismiss() {},
    },
    {
      render: (m, p) => {
        const manager = m.TaskManager(p);
        const card = elements(
          manager,
          (node) => node.type?.name === "TaskCard",
        )[0];
        return card.type(card.props);
      },
    },
  );
  t.after(() => ui.close());
  ui.i18n.configureLocale({ language: "en-US", region: "en-US" });
  ui.replace({});
  assert.match(content(ui.tree), /Copy and verify files/);
  assert.doesNotMatch(content(ui.tree), /untranslated backend label/);
});
