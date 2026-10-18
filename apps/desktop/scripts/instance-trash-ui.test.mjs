/** Actual caller props, native root API and delete/undo handlers. All files,
 * plans, roots and task admissions below are synthetic in-memory projections. */
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { harness, elements, content, deferred } from "./maker-test-harness.mjs";

const require = createRequire(import.meta.url),
  ts = require("typescript");
const source = fileURLToPath(new URL("../src/", import.meta.url));
const mainSource = fs.readFileSync(path.join(source, "main.tsx"), "utf8");
const mainTree = ts.createSourceFile(
  "main.tsx",
  mainSource,
  ts.ScriptTarget.Latest,
  true,
  ts.ScriptKind.TSX,
);
function compiled(code) {
  return ts.transpileModule(code, {
    compilerOptions: {
      target: ts.ScriptTarget.ES2022,
      module: ts.ModuleKind.CommonJS,
      jsx: ts.JsxEmit.ReactJSX,
    },
  }).outputText;
}
function declaration(name) {
  let found;
  function visit(node) {
    if (ts.isVariableDeclaration(node) && node.name.getText(mainTree) === name)
      found = `const ${node.getText(mainTree)};`;
    if (ts.isFunctionDeclaration(node) && node.name?.getText(mainTree) === name)
      found = node.getText(mainTree);
    if (!found) ts.forEachChild(node, visit);
  }
  visit(mainTree);
  assert.ok(found, `Missing main declaration ${name}`);
  return found;
}
function mainTaskTracker() {
  const managed = [],
    revealed = [],
    owner = {};
  const tasks = {
    owner,
    track: (id) => managed.push(id),
    refresh: async () => ({
      tasks: managed.map((id) => ({ task_id: id, stage: "processing" })),
    }),
  };
  const callback = new Function(
    "tasks",
    "contextKey",
    "taskNavigation",
    "taskHasFloatingEntry",
    "showTasks",
    compiled(`${declaration("showInstanceTask")}\nreturn showInstanceTask;`),
  )(
    tasks,
    { current: "root-b:/synthetic/root-b" },
    { current: { epoch: 3 } },
    () => true,
    () => revealed.push(true),
  );
  return { callback, managed, revealed };
}
function rootApiFixture(nativeApi) {
  const events = [];
  let target = null;
  const unexpected = () => {
    throw Error("Delete admission must not reload or register Java");
  };
  const api = new Function(
    "api",
    "rootId",
    "React",
    "contextKey",
    "setResourceTarget",
    "load",
    "acceptJavaRegistration",
    compiled(
      `${declaration("rootCommands")}\n${declaration("resourceWrites")}\n${declaration("rootApi")}\nreturn rootApi;`,
    ),
  )(
    nativeApi,
    "root-a",
    { useMemo: (factory) => factory() },
    { current: "root-a:/synthetic/root-a" },
    (next) => {
      target = typeof next === "function" ? next(target) : next;
      events.push(target);
    },
    unexpected,
    unexpected,
  );
  return { api, events };
}
function mainTrashProps(api, onTaskStart) {
  let attributes;
  function visit(node) {
    if (
      (ts.isJsxOpeningElement(node) || ts.isJsxSelfClosingElement(node)) &&
      node.tagName.getText(mainTree) === "InstanceTrash"
    )
      attributes = node.attributes.properties;
    if (!attributes) ts.forEachChild(node, visit);
  }
  visit(mainTree);
  assert.ok(attributes, "Missing actual InstanceTrash main caller");
  const fields = attributes.map((attribute) => {
    assert.ok(ts.isJsxAttribute(attribute));
    return `${attribute.name.getText(mainTree)}: ${attribute.initializer.expression.getText(mainTree)}`;
  });
  return new Function(
    "rootApi",
    "rootId",
    "rootKey",
    "data",
    "native",
    "archiveAdmission",
    "rootAvailable",
    "showInstanceTask",
    "taskView",
    `return ({${fields.join(",")}});`,
  )(
    api,
    "root-a",
    "root-a:/synthetic/root-a",
    { settings: { root: "/synthetic/root-a", revision: "settings-a" } },
    true,
    { current: { blocked: false, rootAvailable: true } },
    true,
    onTaskStart,
    { revision: "tasks-1" },
  );
}

/** Keep the actual scope hook, unlike generic panel harness placeholders. */
function trashHarness(name, initial) {
  const modules = new Map(),
    hooks = [];
  let props = initial,
    cursor = 0,
    dirty = false,
    pending = [],
    tree,
    closed = false;
  const react = {
    useState(initial) {
      const index = cursor++;
      if (!(index in hooks))
        hooks[index] = typeof initial === "function" ? initial() : initial;
      return [
        hooks[index],
        (next) => {
          const value = typeof next === "function" ? next(hooks[index]) : next;
          if (!Object.is(value, hooks[index])) {
            hooks[index] = value;
            dirty = true;
          }
        },
      ];
    },
    useRef(value) {
      return (hooks[cursor++] ??= { current: value });
    },
    useEffect(effect, dependencies) {
      const index = cursor++,
        previous = hooks[index];
      if (
        !previous ||
        dependencies.some(
          (value, i) => !Object.is(value, previous.dependencies[i]),
        )
      ) {
        hooks[index] = { dependencies };
        pending.push(() => {
          previous?.cleanup?.();
          hooks[index].cleanup = effect();
        });
      }
    },
  };
  function load(file) {
    if (modules.has(file)) return modules.get(file).exports;
    const module = { exports: {} };
    modules.set(file, module);
    function localRequire(id) {
      if (id === "react") return react;
      if (id.endsWith(".css")) return {};
      if (id.startsWith(".")) {
        const base = path.resolve(path.dirname(file), id);
        return load(
          fs.existsSync(`${base}.tsx`) ? `${base}.tsx` : `${base}.ts`,
        );
      }
      return require(id);
    }
    new Function(
      "require",
      "module",
      "exports",
      "document",
      compiled(fs.readFileSync(file, "utf8")),
    )(localRequire, module, module.exports, { activeElement: null });
    return module.exports;
  }
  const component = load(path.join(source, "InstanceTrash.tsx")),
    i18n = load(path.join(source, "i18n.ts"));
  function render() {
    cursor = 0;
    dirty = false;
    pending = [];
    tree = component[name](props);
    for (const effect of pending) effect();
  }
  render();
  return {
    i18n,
    get tree() {
      return tree;
    },
    replace(next) {
      props = { ...props, ...next };
      render();
    },
    async flush() {
      for (let i = 0; i < 40; i++) {
        await new Promise((done) => setImmediate(done));
        if (!dirty || closed) return;
        render();
      }
      throw Error("Delete component did not settle");
    },
    close() {
      closed = true;
      for (const hook of hooks) hook?.cleanup?.();
    },
  };
}
function button(ui, key) {
  const result = elements(
    ui.tree,
    (node) => node.type === "button" && content(node) === ui.i18n.t(key),
  )[0];
  assert.ok(result, `Missing ${key}`);
  return result;
}
function dialog(ui) {
  return elements(
    ui.tree,
    (node) => node.type?.name === "InstanceOperationDialog",
  )[0];
}
const plan = (extra = {}) => ({
  id: "sample",
  root_id: "root-a",
  revision: "delete-plan-a",
  total_files: 7,
  total_bytes: 1024,
  ...extra,
});
const entry = (extra = {}) => ({
  id: "sample",
  root_id: "root-a",
  operation_id: "deleted-a",
  revision: "restore-a",
  created_ms: 1,
  state: "deleted",
  can_restore: true,
  warning: null,
  ...extra,
});
function deleteProps(api, extra = {}) {
  return {
    id: "sample",
    api,
    rootId: "root-a",
    scopeKey: "root-a:settings-a",
    native: true,
    disabled: false,
    onTaskStart() {},
    ...extra,
  };
}

test("actual InstancePanel caller to delete component to main rootApi preserves registry root identity", async () => {
  const calls = [],
    tasks = [];
  const scoped = rootApiFixture(async (command, args) => {
    if (command === "system_info")
      return {
        total_memory_bytes: 16 * 1024 ** 3,
        available_memory_bytes: 8 * 1024 ** 3,
      };
    calls.push({ command, args });
    return command === "instance_delete_prepare"
      ? plan()
      : { id: "delete-task" };
  });
  const panel = harness(
    "InstancePanel",
    {
      instance: {
        id: "sample",
        minecraft_version: "1.21.1",
        loader: "Vanilla",
      },
      section: "overview",
      settings: {
        root_id: "root-a",
        root: "/synthetic/root-a",
        revision: "settings-a",
        memory_gib: 6,
        overrides: {},
      },
      api: scoped.api,
      native: true,
      disabled: false,
      mutationDisabled: false,
      logsDisabled: false,
      onSave() {},
      onOpen() {},
      onInspect() {},
      onLogs() {},
      onTestLaunch() {},
      onNotify() {},
      onTaskStart: (...args) => tasks.push(args),
    },
    { module: "InstancesPanel.tsx" },
  );
  const child = elements(
    panel.tree,
    (node) => node.type?.name === "InstanceDelete",
  )[0];
  assert.ok(child);
  assert.equal(child.props.rootId, "root-a");
  assert.equal(child.props.scopeKey, "root-a:settings-a");
  const ui = trashHarness("InstanceDelete", child.props);
  button(ui, "trash.deleteInstance").props.onClick();
  await ui.flush();
  assert.equal(dialog(ui).props.confirmLabel, ui.i18n.t("ui.delete"));
  dialog(ui).props.onConfirm();
  await ui.flush();
  assert.deepEqual(calls, [
    {
      command: "instance_delete_prepare",
      args: { id: "sample", rootId: "root-a" },
    },
    {
      command: "instance_delete_start",
      args: { id: "sample", revision: "delete-plan-a", rootId: "root-a" },
    },
  ]);
  assert.deepEqual(tasks, [["delete-task", true]]);
  assert.deepEqual(scoped.events, [{ id: "root-a" }, null]);
  ui.close();
  panel.close();
});

test("actual main trash caller lists and restores the registered root with a separate lifecycle key", async () => {
  const calls = [],
    tasks = [];
  const scoped = rootApiFixture(async (command, args) => {
    calls.push({ command, args });
    return command === "instance_deleted_list"
      ? [entry()]
      : { id: "restore-task" };
  });
  const props = mainTrashProps(scoped.api, (...args) => tasks.push(args));
  assert.equal(props.rootId, "root-a");
  assert.notEqual(props.scopeKey, props.rootId);
  const ui = trashHarness("InstanceTrash", props);
  await ui.flush();
  button(ui, "trash.undo").props.onClick();
  await ui.flush();
  assert.deepEqual(calls, [
    { command: "instance_deleted_list", args: { rootId: "root-a" } },
    {
      command: "instance_restore_start",
      args: {
        operationId: "deleted-a",
        revision: "restore-a",
        rootId: "root-a",
      },
    },
  ]);
  assert.deepEqual(tasks, [["restore-task", true]]);
  ui.close();
});

test("foreign delete plans and restore records retain identity validation", async () => {
  const ui = trashHarness(
    "InstanceDelete",
    deleteProps(async () => plan({ root_id: "foreign-root" })),
  );
  button(ui, "trash.deleteInstance").props.onClick();
  await ui.flush();
  assert.ok(content(dialog(ui)).includes(ui.i18n.t("trash.planChanged")));
  ui.close();
  const trash = trashHarness(
    "InstanceTrash",
    deleteProps(async () => [entry({ root_id: "foreign-root" })]),
  );
  await trash.flush();
  assert.ok(content(trash.tree).includes(trash.i18n.t("trash.recordChanged")));
  assert.equal(
    elements(
      trash.tree,
      (node) =>
        node.type === "button" && content(node) === trash.i18n.t("trash.undo"),
    ).length,
    0,
  );
  trash.close();
});

test("closed or retired prepare reads cannot revive a delete confirmation", async () => {
  for (const retire of ["close", "root", "revision", "api"]) {
    const pending = deferred();
    const ui = trashHarness(
      "InstanceDelete",
      deleteProps(() => pending.promise),
    );
    button(ui, "trash.deleteInstance").props.onClick();
    await ui.flush();
    if (retire === "close") dialog(ui).props.onClose();
    if (retire === "root") ui.replace({ rootId: "root-b" });
    if (retire === "revision") ui.replace({ scopeKey: "root-a:settings-b" });
    if (retire === "api") ui.replace({ api: async () => plan() });
    pending.resolve(plan());
    await ui.flush();
    assert.equal(dialog(ui), undefined, retire);
    ui.close();
  }
});

test("a saved confirmation cannot revive after root A to B to A", async () => {
  const calls = [];
  const ui = trashHarness(
    "InstanceDelete",
    deleteProps(async (command) => {
      calls.push(command);
      return plan();
    }),
  );
  button(ui, "trash.deleteInstance").props.onClick();
  await ui.flush();
  const oldConfirm = dialog(ui).props.onConfirm;
  ui.replace({ rootId: "root-b" });
  ui.replace({ rootId: "root-a" });
  oldConfirm();
  await ui.flush();
  assert.deepEqual(calls, ["instance_delete_prepare"]);
  ui.close();
});

test("admitted deletion stays managed after navigation or unmount, using its captured callback", async () => {
  for (const retire of ["root", "revision", "unmount"]) {
    const pending = deferred(),
      original = [],
      newer = [],
      calls = [];
    const manager = mainTaskTracker();
    const ui = trashHarness(
      "InstanceDelete",
      deleteProps(
        async (command) => {
          calls.push(command);
          return command === "instance_delete_prepare"
            ? plan()
            : pending.promise;
        },
        {
          onTaskStart: (...args) => {
            original.push(args);
            return manager.callback(...args);
          },
        },
      ),
    );
    button(ui, "trash.deleteInstance").props.onClick();
    await ui.flush();
    const confirm = dialog(ui).props.onConfirm;
    confirm();
    confirm();
    await ui.flush();
    if (retire === "root")
      ui.replace({
        rootId: "root-b",
        onTaskStart: (...args) => newer.push(args),
      });
    if (retire === "revision")
      ui.replace({
        scopeKey: "root-a:settings-b",
        onTaskStart: (...args) => newer.push(args),
      });
    if (retire === "unmount") ui.close();
    pending.resolve({ id: "accepted-delete" });
    await ui.flush();
    assert.deepEqual(original, [["accepted-delete", false]], retire);
    assert.deepEqual(newer, []);
    assert.deepEqual(manager.managed, ["accepted-delete"]);
    assert.deepEqual(manager.revealed, []);
    assert.deepEqual(calls, [
      "instance_delete_prepare",
      "instance_delete_start",
    ]);
    ui.close();
  }
});

test("admitted restoration stays managed after navigation or unmount", async () => {
  for (const retire of ["root", "unmount"]) {
    const pending = deferred(),
      tasks = [];
    const manager = mainTaskTracker();
    const ui = trashHarness(
      "InstanceTrash",
      deleteProps(
        async (command) =>
          command === "instance_deleted_list" ? [entry()] : pending.promise,
        {
          onTaskStart: (...args) => {
            tasks.push(args);
            return manager.callback(...args);
          },
        },
      ),
    );
    await ui.flush();
    button(ui, "trash.undo").props.onClick();
    await ui.flush();
    if (retire === "root") ui.replace({ rootId: "root-b" });
    else ui.close();
    pending.resolve({ id: "accepted-restore" });
    await ui.flush();
    assert.deepEqual(tasks, [["accepted-restore", false]], retire);
    assert.deepEqual(manager.managed, ["accepted-restore"]);
    assert.deepEqual(manager.revealed, []);
    ui.close();
  }
});

test("disabled or stale restore handlers cannot replay another scope's record", async () => {
  const calls = [];
  const ui = trashHarness(
    "InstanceTrash",
    deleteProps(async (command) => {
      calls.push(command);
      return [entry()];
    }),
  );
  await ui.flush();
  const restore = button(ui, "trash.undo").props.onClick;
  ui.replace({ disabled: true });
  restore();
  await ui.flush();
  assert.deepEqual(calls, ["instance_deleted_list"]);
  ui.replace({ rootId: "root-b", disabled: false });
  ui.replace({ rootId: "root-a" });
  await ui.flush();
  restore();
  await ui.flush();
  assert.equal(
    calls.filter((command) => command === "instance_restore_start").length,
    0,
  );
  ui.close();
});
