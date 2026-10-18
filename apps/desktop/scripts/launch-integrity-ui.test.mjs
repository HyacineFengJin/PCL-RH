/** Synthetic native projections exercising the actual integrity UI handlers. */
import assert from "node:assert/strict";
import fs from "node:fs";
import { createRequire } from "node:module";
import { test } from "node:test";
import {
  harness,
  elements,
  content,
  button,
  deferred,
} from "./maker-test-harness.mjs";

const require = createRequire(import.meta.url),
  ts = require("typescript"),
  lifecycle = { exports: {} };
new Function(
  "module",
  "exports",
  ts.transpileModule(
    fs.readFileSync(
      new URL("../src/taskLifecycle.ts", import.meta.url),
      "utf8",
    ),
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    },
  ).outputText,
)(lifecycle, lifecycle.exports);

const mainSource = fs.readFileSync(
    new URL("../src/main.tsx", import.meta.url),
    "utf8",
  ),
  mainTree = ts.createSourceFile(
    "main.tsx",
    mainSource,
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.TSX,
  );
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
  assert.ok(found, `Missing actual main declaration ${name}`);
  return found;
}
function compiled(code) {
  return ts.transpileModule(code, {
    compilerOptions: {
      target: ts.ScriptTarget.ES2022,
      module: ts.ModuleKind.CommonJS,
    },
  }).outputText;
}
function rootApiFixture(api) {
  const events = [];
  let resourceTarget = null;
  const unexpected = () => {
    throw Error(
      "Integrity admission must not reload bootstrap or register Java",
    );
  };
  const scoped = new Function(
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
    api,
    "root-a",
    { useMemo: (factory) => factory() },
    { current: "root-a:sample" },
    (next) => {
      resourceTarget = typeof next === "function" ? next(resourceTarget) : next;
      events.push(resourceTarget);
    },
    unexpected,
    unexpected,
  );
  return { api: scoped, events };
}
function mainDismiss(tasks, dismissedSnapshot, setDismissedTasks) {
  return new Function(
    "tasks",
    "dismissedSnapshot",
    "setDismissedTasks",
    compiled(`${declaration("dismissTask")}\nreturn dismissTask;`),
  )(tasks, dismissedSnapshot, setDismissedTasks);
}
function completionEffect() {
  let body;
  function visit(node) {
    if (
      ts.isCallExpression(node) &&
      node.expression.getText(mainTree) === "useEffect" &&
      node.arguments[0]
        ?.getText(mainTree)
        .includes("taskPageOwner.current.takeCompletion(visibleTasks)")
    )
      body = node.arguments[0].getText(mainTree);
    if (!body) ts.forEachChild(node, visit);
  }
  visit(mainTree);
  assert.ok(body, "Missing actual main task completion effect");
  return compiled(`return (${body})();`);
}

function inspection() {
  return {
    id: "sample",
    minecraft_version: "1.21.1",
    loader: "Vanilla",
    game_dir: "/synthetic/root",
    java_major: 21,
    java_exact: false,
    java_path: "/synthetic/java",
    java_error: null,
    missing_files: [],
    missing_total: 0,
    issues: [],
    ready: true,
  };
}
function report(extra = {}) {
  return {
    id: "sample",
    files_verified: 4,
    files_missing: 1,
    files_corrupt: 2,
    files_unverifiable: 1,
    assets_enumerated: false,
    problems_total: 40,
    problems: Array.from({ length: 32 }, (_, n) => ({
      path: `libraries/synthetic-${n}.jar`,
      kind: n % 2 ? "missing" : "sha1_mismatch",
      reason: `synthetic reason ${n}`,
    })),
    ...extra,
  };
}
function task(extra = {}) {
  return {
    task_id: "verify-task",
    kind: "instance_verify",
    root_id: "root-a",
    version: "sample",
    stage: "complete",
    phase: "complete",
    message: "Check completed",
    completed: 8,
    total: 8,
    bytes_done: 100,
    bytes_total: 100,
    network_bytes: 0,
    can_cancel: false,
    result: report(),
    ...extra,
  };
}
async function dialog() {
  const calls = [],
    pending = deferred(),
    events = [];
  const ui = harness("LaunchInspection", {
    id: "sample",
    scopeKey: "root-a",
    revision: "revision-a",
    native: true,
    disabled: false,
    api: async (command, args) => {
      calls.push({ command, args });
      return command === "inspect_instance" ? inspection() : pending.promise;
    },
    onClose: () => events.push("close"),
    onManageJava() {},
    onTaskStart: (id, reveal) => events.push({ id, reveal }),
  });
  await ui.flush();
  return { ui, calls, pending, events };
}
function labelled(ui, key) {
  return elements(
    ui.tree,
    (node) =>
      node.type === "button" && node.props["aria-label"] === ui.i18n.t(key),
  )[0];
}

test("a present-file inspection can explicitly verify with the captured revision once", async () => {
  const { ui, calls, pending, events } = await dialog();
  assert.ok(content(ui.tree).includes(ui.i18n.t("integrity.help")));
  const start = button(ui, "integrity.open").props.onClick;
  start();
  start();
  button(ui, "inspection.recheck").props.onClick();
  await ui.flush();
  assert.deepEqual(calls, [
    { command: "inspect_instance", args: { id: "sample" } },
    {
      command: "instance_verify_start",
      args: { id: "sample", revision: "revision-a" },
    },
  ]);
  assert.equal(button(ui, "integrity.starting").props.disabled, true);
  pending.resolve({ id: "verify-task" });
  await ui.flush();
  assert.deepEqual(events, ["close", { id: "verify-task", reveal: true }]);
  ui.close();
});

test("actual main rootApi forwards root/id/revision and retains admission until acknowledgement", async () => {
  const pending = deferred(),
    calls = [],
    events = [];
  const scoped = rootApiFixture(async (command, args) => {
    calls.push({ command, args });
    return command === "inspect_instance" ? inspection() : pending.promise;
  });
  const ui = harness("LaunchInspection", {
    id: "sample",
    scopeKey: "root-a",
    revision: "revision-a",
    native: true,
    disabled: false,
    api: scoped.api,
    onClose: () => events.push("close"),
    onManageJava() {},
    onTaskStart: (id, reveal) => events.push({ id, reveal }),
  });
  await ui.flush();
  button(ui, "integrity.open").props.onClick();
  await ui.flush();
  assert.deepEqual(calls, [
    { command: "inspect_instance", args: { id: "sample", rootId: "root-a" } },
    {
      command: "instance_verify_start",
      args: { id: "sample", revision: "revision-a", rootId: "root-a" },
    },
  ]);
  assert.deepEqual(scoped.events, [{ id: "root-a" }]);
  pending.resolve({ id: "verify-task" });
  await ui.flush();
  assert.deepEqual(scoped.events, [{ id: "root-a" }, null]);
  assert.deepEqual(events, ["close", { id: "verify-task", reveal: true }]);
  ui.close();
});

test("late verification admission is still tracked after modal close", async () => {
  const { ui, pending, events } = await dialog();
  button(ui, "integrity.open").props.onClick();
  labelled(ui, "inspection.close").props.onClick();
  ui.close();
  pending.resolve({ id: "late-verify" });
  await ui.flush();
  assert.deepEqual(events, ["close", { id: "late-verify", reveal: false }]);
});

test("saved verify actions retire on disablement or instance revision replacement", async () => {
  for (const next of [
    { disabled: true },
    { revision: "revision-b" },
    { id: "other" },
  ]) {
    const { ui, calls } = await dialog();
    const start = button(ui, "integrity.open").props.onClick;
    ui.replace(next);
    start();
    await ui.flush();
    assert.equal(
      calls.filter((call) => call.command === "instance_verify_start").length,
      0,
    );
    ui.close();
  }
});

test("integrity start errors retain an explicit retry action", async () => {
  const { ui, pending } = await dialog();
  button(ui, "integrity.open").props.onClick();
  pending.reject(new Error("synthetic binding changed"));
  await ui.flush();
  assert.ok(content(ui.tree).includes(ui.i18n.t("integrity.failed")));
  assert.ok(content(ui.tree).includes("synthetic binding changed"));
  assert.equal(button(ui, "integrity.open").props.disabled, false);
  ui.close();
});

test("completed verification remains reachable and prevents automatic result dismissal", () => {
  const { taskHasFloatingEntry, TaskPageOwner, taskNeedsBootstrap } =
    lifecycle.exports;
  const result = task(),
    owner = new TaskPageOwner();
  assert.equal(owner.open(task({ stage: "processing", result: null })), true);
  assert.equal(owner.takeCompletion([result]), false);
  assert.equal(taskHasFloatingEntry(result), true);
  assert.equal(taskNeedsBootstrap(result, "root-a"), false);
  assert.equal(taskHasFloatingEntry(task({ kind: "install" })), false);
  assert.equal(owner.takeCompletion([]), true);
  assert.equal(owner.completion[0].task_id, "verify-task");
});

test("actual main retains the completed report then dismisses it and returns to the task origin", () => {
  const { TaskPageOwner, taskHasFloatingEntry, taskIsActive } =
    lifecycle.exports;
  const owner = new TaskPageOwner(),
    completed = task(),
    taskView = { tasks: [completed] },
    tasks = { snapshot: { current: taskView } },
    dismissedSnapshot = { current: new Set() };
  let screen = "tasks",
    dismissed = new Set(),
    loads = 0;
  owner.open(task({ stage: "processing", result: null }));
  const effect = new Function(
    "screen",
    "tasks",
    "taskView",
    "taskPageOwner",
    "visibleTasks",
    "taskOrigin",
    "load",
    "setTab",
    "setDownloadPage",
    "setTaskOrigin",
    "setScreen",
    "dismissedSnapshot",
    "taskHasFloatingEntry",
    "taskIsActive",
    "notify",
    "t",
    completionEffect(),
  );
  const run = () =>
    effect(
      screen,
      tasks,
      taskView,
      { current: owner },
      taskView.tasks.filter((value) => !dismissed.has(value.task_id)),
      "instance",
      () => loads++,
      () => {},
      () => {},
      () => {},
      (next) => {
        screen = typeof next === "function" ? next(screen) : next;
      },
      dismissedSnapshot,
      taskHasFloatingEntry,
      taskIsActive,
      () => {},
      (key) => key,
    );
  run();
  assert.equal(screen, "tasks");
  assert.equal(loads, 0);
  assert.equal(taskHasFloatingEntry(completed), true);
  const dismiss = mainDismiss(tasks, dismissedSnapshot, (next) => {
    dismissed = next;
  });
  dismiss(completed);
  assert.equal(dismissed.has("verify-task"), true);
  run();
  assert.equal(screen, "instance");
  assert.equal(loads, 0);
  assert.equal(completed.result.problems[0].path, "libraries/synthetic-0.jar");
});

test("actual main dismissal rejects a running task, missing ID and ordinary completed install", () => {
  for (const current of [
    task({ stage: "processing" }),
    task({ kind: "install" }),
    task({ task_id: "other" }),
  ]) {
    const dismissedSnapshot = { current: new Set() },
      values = [];
    mainDismiss(
      { snapshot: { current: { tasks: [current] } } },
      dismissedSnapshot,
      (next) => values.push(next),
    )(task());
    assert.deepEqual(values, []);
  }
});

test("complete verification cards retain the result and dismiss without cancelling", () => {
  const events = [],
    calls = [],
    status = task();
  const ui = harness(
    "TaskManager",
    {
      status,
      native: true,
      api: async (command) => calls.push(command),
      onNotify() {},
      onDismiss: (value) => events.push(value),
    },
    {
      render(component, props) {
        const manager = component.TaskManager(props);
        const card = elements(
          manager,
          (node) => node.type?.name === "TaskCard",
        )[0];
        return card.type(card.props);
      },
    },
  );
  assert.ok(
    elements(ui.tree, (node) => node.type?.name === "IntegrityResult").length,
  );
  assert.ok(content(ui.tree).includes(ui.i18n.t("integrity.open")));
  for (const language of ["zh-CN", "en-US"]) {
    const translator = ui.i18n.createTranslator({ language, region: language });
    ui.i18n.t = translator.t;
    ui.replace({});
    const dismiss = labelled(ui, "integrity.dismiss");
    assert.ok(dismiss, `Missing ${language} verification-result dismissal`);
    assert.equal(dismiss.props.title, translator.t("integrity.dismiss"));
    assert.equal(labelled(ui, "task.dismiss"), undefined);
  }
  labelled(ui, "integrity.dismiss").props.onClick();
  assert.deepEqual(events, [status]);
  assert.deepEqual(calls, []);
  ui.replace({ status: { ...status, stage: "error" } });
  assert.ok(labelled(ui, "task.dismiss"));
  assert.equal(labelled(ui, "integrity.dismiss"), undefined);
  ui.close();
});

test("readable results show counts, damaged paths, omitted findings and incomplete coverage", () => {
  for (const language of ["zh-CN", "en-US"]) {
    const result = report();
    const ui = harness(
      "IntegrityResult",
      { result },
      {
        render(component, props) {
          return component.IntegrityResult(props);
        },
      },
    );
    const translator = ui.i18n.createTranslator({ language, region: language });
    ui.i18n.t = translator.t;
    ui.i18n.formatNumber = translator.formatNumber;
    ui.replace({ result });
    const text = content(ui.tree);
    for (const key of [
      "integrity.verified",
      "integrity.missing",
      "integrity.corrupt",
      "integrity.unverifiable",
      "integrity.coverage",
      "integrity.complete",
    ])
      assert.ok(text.includes(ui.i18n.t(key)), `${language} ${key}`);
    assert.equal(elements(ui.tree, (node) => node.type === "li").length, 32);
    assert.ok(text.includes("libraries/synthetic-31.jar"));
    assert.ok(text.includes(ui.i18n.t("integrity.omitted", { count: "8" })));
    assert.equal(
      elements(ui.tree, (node) => node.type === "details")[0].props.open,
      true,
    );
    ui.close();
  }
});

test("malformed integrity projections never appear as a clean check", () => {
  for (const result of [
    null,
    { ...report(), files_corrupt: -1 },
    { ...report(), problems: [{ path: "x", reason: "x", kind: "unknown" }] },
  ]) {
    const ui = harness("IntegrityResult", { result });
    assert.ok(
      content(ui.tree).includes(ui.i18n.t("integrity.resultUnavailable")),
    );
    ui.close();
  }
});
