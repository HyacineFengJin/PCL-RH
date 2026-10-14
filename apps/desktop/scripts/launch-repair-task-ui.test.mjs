/** Repair task cards and lifecycle checks with synthetic native projections. */
import assert from "node:assert/strict";
import fs from "node:fs";
import { createRequire } from "node:module";
import { test } from "node:test";
import { harness, elements, content, deferred } from "./maker-test-harness.mjs";

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
const { taskNeedsBootstrap, TaskPageOwner, taskHasFloatingEntry } =
  lifecycle.exports;

function task(extra = {}) {
  return {
    task_id: "repair-task-a",
    root_id: "root-a",
    root_path: "/synthetic/root-a",
    kind: "instance_repair",
    version: "sample",
    stage: "error",
    phase: "repair-verify",
    message: "Some files require manual attention",
    error: "Unsupported dependency metadata",
    completed: 8,
    total: 8,
    bytes_done: 1024,
    bytes_total: 1024,
    progress: 1,
    can_cancel: false,
    result: {
      id: "sample",
      files_downloaded: 8,
      files_present: 20,
      unsupported_total: 40,
      unsupported: Array.from({ length: 32 }, (_, index) => ({
        path: `/synthetic/libraries/manual-${index}.jar`,
        reason: `Missing checksum ${index}`,
      })),
    },
    ...extra,
  };
}
function card(status = task(), extra = {}) {
  const calls = [],
    events = [],
    pending = deferred();
  const ui = harness(
    "TaskManager",
    {
      status,
      native: true,
      api: async (command, args) => {
        calls.push({ command, args });
        return pending.promise;
      },
      onNotify: (message) => events.push({ type: "notify", message }),
      onStatusChange: (status) => events.push({ type: "status", status }),
      onCancelled: (status) => events.push({ type: "cancelled", status }),
      onDismiss: (status) => events.push({ type: "dismiss", status }),
      ...extra,
    },
    {
      render(component, props) {
        const manager = component.TaskManager(props);
        // Render the real private card through its returned React element.
        const child = elements(
          manager,
          (node) => node.type?.name === "TaskCard",
        )[0];
        return {
          ...manager,
          props: {
            ...manager.props,
            children: [
              child.type(child.props),
              ...elements(
                manager,
                (node) => node.props?.className === "ce-card ce-task-empty",
              ),
            ],
          },
        };
      },
    },
  );
  return { ui, calls, pending, events };
}
function labelledButton(ui, label) {
  const found = elements(
    ui.tree,
    (node) => node.type === "button" && node.props["aria-label"] === label,
  )[0];
  assert.ok(found, `Missing button ${label}`);
  return found;
}

test("partial repair errors retain their report and localized task/phase labels", async () => {
  for (const [language, labels] of [
    [
      "zh-CN",
      [
        "补全缺失文件",
        "检查缺失文件",
        "下载并校验缺失文件",
        "复核实例文件",
        "40 个文件需手动处理",
      ],
    ],
    [
      "en-US",
      [
        "Download missing files",
        "Check missing files",
        "Download and verify missing files",
        "Recheck instance files",
        "40 files need manual attention",
      ],
    ],
  ]) {
    const status = task({
      steps: [
        { id: "repair-check", label: "native plan", state: "complete" },
        { id: "repair-download", label: "native download", state: "complete" },
        { id: "repair-verify", label: "native verify", state: "running" },
      ],
    });
    const { ui, calls, events } = card(status);
    const translator = ui.i18n.createTranslator({ language, region: language });
    ui.i18n.t = translator.t;
    ui.i18n.formatNumber = translator.formatNumber;
    ui.replace({});
    await ui.flush();
    const text = content(ui.tree);
    assert.ok(
      elements(
        ui.tree,
        (node) => node.props?.className === "ce-card ce-task-card error",
      ).length,
    );
    assert.equal(
      elements(
        ui.tree,
        (node) => node.props?.className === "ce-card ce-task-empty",
      ).length,
      0,
    );
    for (const label of [...labels, status.message, status.error])
      assert.ok(text.includes(label), `Missing ${language} text ${label}`);
    assert.ok(!text.includes("native plan"));
    assert.ok(!text.includes("native download"));
    assert.ok(!text.includes("native verify"));
    const details = elements(ui.tree, (node) => node.type === "details")[0];
    assert.equal(details.props.open, undefined);
    assert.equal(elements(details, (node) => node.type === "li").length, 32);
    for (const item of status.result.unsupported) {
      assert.ok(text.includes(item.path));
      assert.ok(text.includes(item.reason));
    }
    assert.ok(text.includes(ui.i18n.t("inspection.moreFiles", { count: "8" })));
    labelledButton(ui, ui.i18n.t("task.dismiss")).props.onClick();
    assert.deepEqual(events, [{ type: "dismiss", status }]);
    assert.deepEqual(calls, []);
    ui.close();
  }
});

test("repair cards bound full reports to 32 path/reason rows and count all omitted entries", async () => {
  const status = task();
  status.result.unsupported = Array.from({ length: 40 }, (_, index) => ({
    path: `/synthetic/libraries/full-${index}.jar`,
    reason: `Manual reason ${index}`,
  }));
  const { ui } = card(status);
  await ui.flush();
  const text = content(ui.tree);
  assert.equal(elements(ui.tree, (node) => node.type === "li").length, 32);
  assert.ok(text.includes("/synthetic/libraries/full-31.jar"));
  assert.ok(text.includes("Manual reason 31"));
  assert.ok(!text.includes("/synthetic/libraries/full-32.jar"));
  assert.ok(!text.includes("Manual reason 32"));
  assert.ok(text.includes(ui.i18n.t("inspection.moreFiles", { count: "8" })));
  for (const key of [
    "inspection.repairPlan",
    "inspection.repairFiles",
    "inspection.repairCheck",
  ])
    assert.ok(text.includes(ui.i18n.t(key)));
  ui.close();
});

test("repair cancellation targets its task ID and waits for cleanup; unmount alone does not cancel", async () => {
  const running = task({
    stage: "downloading",
    phase: "repair-download",
    can_cancel: true,
    result: null,
  });
  const { ui, calls, pending, events } = card(running);
  const label = ui.i18n.t("task.cancel", {
    action: ui.i18n.t("inspection.repair"),
  });
  labelledButton(ui, label).props.onClick();
  await ui.flush();
  assert.deepEqual(calls, [
    { command: "download_cancel", args: { taskId: "repair-task-a" } },
  ]);
  assert.deepEqual(events, []);
  assert.equal(
    labelledButton(ui, ui.i18n.t("task.cleanupPending")).props.disabled,
    true,
  );
  assert.ok(content(ui.tree).includes(ui.i18n.t("task.cancelling")));
  const cancelled = {
    ...running,
    stage: "cancelled",
    message: "Cleanup complete",
  };
  pending.resolve(cancelled);
  await ui.flush();
  assert.deepEqual(events, [
    { type: "status", status: cancelled },
    { type: "cancelled", status: cancelled },
  ]);
  ui.close();
  const closed = card(running);
  closed.ui.close();
  assert.deepEqual(closed.calls, []);
});

test("only terminal repair outcomes with the current root identity refresh bootstrap", () => {
  for (const stage of [
    "idle",
    "queued",
    "preparing",
    "downloading",
    "processing",
    "complete",
    "error",
    "cancelled",
  ]) {
    const terminal = ["complete", "error", "cancelled"].includes(stage);
    const status = task({ stage });
    assert.equal(
      taskNeedsBootstrap(status, "root-a"),
      terminal,
      `Current root ${stage}`,
    );
    assert.equal(
      taskNeedsBootstrap(status, "root-b"),
      false,
      `Another root ${stage}`,
    );
    assert.equal(
      taskNeedsBootstrap(status, null),
      false,
      `No current root ${stage}`,
    );
    assert.equal(
      taskNeedsBootstrap({ ...status, root_id: null }, null),
      false,
      `Unidentified root ${stage}`,
    );
  }
});

test("TaskPageOwner retains partial repair Error instead of treating completed download counts as success", () => {
  const owner = new TaskPageOwner(),
    running = task({ stage: "downloading", progress: 0.5, result: null }),
    partial = task();
  assert.equal(owner.open(running), true);
  assert.equal(owner.takeCompletion(partial), false);
  assert.equal(taskHasFloatingEntry(partial), true);
  assert.deepEqual(owner.completion, []);
  assert.equal(owner.takeCompletion(partial), false);
  assert.deepEqual(owner.completion, []);
  const errorOnly = new TaskPageOwner();
  assert.equal(errorOnly.open(partial), true);
  assert.equal(errorOnly.takeCompletion(partial), false);
  const completeOwner = new TaskPageOwner(),
    complete = {
      ...partial,
      stage: "complete",
      error: null,
      result: { ...partial.result, unsupported_total: 0, unsupported: [] },
    };
  assert.equal(completeOwner.open(running), true);
  assert.equal(completeOwner.takeCompletion(complete), true);
  assert.deepEqual(completeOwner.completion, [complete]);
});
