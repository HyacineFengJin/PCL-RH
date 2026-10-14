/** Actual repair handlers with synthetic reports and native task replies. */
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  harness,
  elements,
  content,
  button,
  deferred,
} from "./maker-test-harness.mjs";

function report(id = "sample", extra = {}) {
  return {
    id,
    minecraft_version: "1.21.1",
    loader: "Fabric",
    game_dir: "/synthetic/root/versions/sample",
    java_major: 21,
    java_exact: false,
    java_path: "/synthetic/java/bin/java",
    java_error: null,
    missing_files: ["/synthetic/libraries/missing.jar"],
    missing_total: 1,
    issues: [],
    ready: false,
    ...extra,
  };
}
function dialog(extra = {}) {
  const calls = [],
    requests = [],
    events = [];
  const api = async (command, args) => {
    calls.push({ command, args });
    const request = deferred();
    requests.push(request);
    return request.promise;
  };
  const ui = harness("LaunchInspection", {
    id: "sample",
    scopeKey: "root-a",
    api,
    native: true,
    disabled: false,
    revision: "settings-revision-a",
    onClose() {
      events.push({ type: "close" });
    },
    onManageJava() {},
    onTaskStart(id, reveal) {
      events.push({ type: "task", id, reveal });
    },
    ...extra,
  });
  return { ui, api, calls, requests, events };
}
async function inspected(extra = {}, details = {}) {
  const fixture = dialog(extra);
  fixture.requests[0].resolve(report(fixture.ui.props.id, details));
  await fixture.ui.flush();
  return fixture;
}
function hasButton(ui, key) {
  return elements(
    ui.tree,
    (node) => node.type === "button" && content(node) === ui.i18n.t(key),
  ).length;
}

test("repair submits the exact instance revision once and closes before revealing the task", async () => {
  const { ui, calls, requests, events } = await inspected();
  assert.ok(content(ui.tree).includes(ui.i18n.t("inspection.repairHelp")));
  const start = button(ui, "inspection.repair").props.onClick,
    recheck = button(ui, "inspection.recheck").props.onClick;
  start();
  start();
  recheck();
  await ui.flush();
  assert.deepEqual(calls, [
    { command: "inspect_instance", args: { id: "sample" } },
    {
      command: "instance_repair_start",
      args: { id: "sample", revision: "settings-revision-a" },
    },
  ]);
  assert.equal(button(ui, "inspection.repairStarting").props.disabled, true);
  assert.equal(button(ui, "inspection.recheck").props.disabled, true);
  assert.equal(
    elements(ui.tree, (node) => node.props?.role === "dialog")[0].props[
      "aria-busy"
    ],
    true,
  );
  assert.deepEqual(events, []);
  requests[1].resolve({ id: "repair-task" });
  await ui.flush();
  assert.deepEqual(events, [
    { type: "close" },
    { type: "task", id: "repair-task", reveal: true },
  ]);
  start();
  recheck();
  assert.equal(calls.length, 2);
  ui.close();
});

test("only a missing-file report without configuration issues offers repair", async () => {
  for (const details of [
    { missing_total: 0, missing_files: [], ready: true },
    { issues: ["Dependency metadata could not be read"] },
  ]) {
    const { ui, calls } = await inspected({}, details);
    assert.equal(hasButton(ui, "inspection.repair"), 0);
    assert.equal(calls.length, 1);
    ui.close();
  }
  const { ui } = await inspected(
    {},
    { java_path: null, java_error: "Java is unavailable" },
  );
  assert.equal(button(ui, "inspection.repair").props.disabled, false);
  button(ui, "inspection.manageJava");
  ui.close();
});

test("missing or whitespace revision cannot submit a repair", async () => {
  for (const revision of [undefined, "", "   "]) {
    const { ui, calls } = await inspected({ revision });
    const repair = button(ui, "inspection.repair");
    assert.equal(repair.props.disabled, true);
    repair.props.onClick();
    await ui.flush();
    assert.equal(calls.length, 1);
    ui.close();
  }
});

test("closing or unmounting during submission still tracks late task success without reveal", async () => {
  for (const exit of ["close", "unmount"]) {
    const { ui, calls, requests, events } = await inspected();
    const start = button(ui, "inspection.repair").props.onClick;
    start();
    await ui.flush();
    if (exit === "close") {
      const close = button(ui, "inspection.close").props.onClick;
      assert.equal(button(ui, "inspection.close").props.disabled, undefined);
      close();
      close();
    }
    ui.close();
    requests[1].resolve({ id: "late-task" });
    await ui.flush();
    start();
    assert.equal(calls.length, 2);
    assert.deepEqual(events, [
      ...(exit === "close" ? [{ type: "close" }] : []),
      { type: "task", id: "late-task", reveal: false },
    ]);
  }
});

test("root, instance, revision, and API changes track the captured task without navigating back", async () => {
  for (const changed of [
    { scopeKey: "root-b" },
    { id: "other" },
    { revision: "settings-revision-b" },
    { api: async () => report("sample", { game_dir: "new API report" }) },
  ]) {
    const { ui, calls, requests, events } = await inspected();
    const start = button(ui, "inspection.repair").props.onClick;
    start();
    ui.replace({
      ...changed,
      onTaskStart(id, reveal) {
        events.push({ type: "replacement-task", id, reveal });
      },
    });
    await ui.flush();
    start();
    assert.equal(
      calls.filter((call) => call.command === "instance_repair_start").length,
      1,
    );
    requests[1].resolve({ id: "captured-task" });
    await ui.flush();
    assert.deepEqual(events, [
      { type: "task", id: "captured-task", reveal: false },
    ]);
    if (requests[2]) {
      requests[2].resolve(report(changed.id || "sample"));
      await ui.flush();
    }
    assert.equal(hasButton(ui, "inspection.repair"), 1);
    assert.equal(button(ui, "inspection.repair").props.disabled, false);
    ui.close();
  }
});

test("a callback replacement within the same scope preserves the submission owner", async () => {
  const { ui, requests, events } = await inspected();
  button(ui, "inspection.repair").props.onClick();
  ui.replace({
    onTaskStart(id, reveal) {
      events.push({ type: "replacement-task", id, reveal });
    },
  });
  requests[1].resolve({ id: "same-scope-task" });
  await ui.flush();
  assert.deepEqual(events, [
    { type: "close" },
    { type: "task", id: "same-scope-task", reveal: true },
  ]);
  ui.close();
});

test("saved repair actions from another root, instance, API, or revision cannot submit", async () => {
  for (const changed of [
    { scopeKey: "root-b" },
    { id: "other" },
    { revision: "settings-revision-b" },
    { api: async () => report("sample") },
  ]) {
    const { ui, calls, requests } = await inspected();
    const oldRepair = button(ui, "inspection.repair").props.onClick;
    ui.replace(changed);
    oldRepair();
    await ui.flush();
    if (requests[1]) {
      requests[1].resolve(report(changed.id || "sample"));
      await ui.flush();
    }
    oldRepair();
    assert.equal(
      calls.filter((call) => call.command === "instance_repair_start").length,
      0,
    );
    ui.close();
  }
});

test("repair submission disables Java navigation and rejects its saved handler", async () => {
  const navigation = [];
  const { ui, requests, events } = await inspected(
    { onManageJava: (major) => navigation.push(major) },
    { java_path: null, java_error: "Java is unavailable" },
  );
  const manageJava = button(ui, "inspection.manageJava").props.onClick;
  button(ui, "inspection.repair").props.onClick();
  manageJava();
  await ui.flush();
  assert.equal(button(ui, "inspection.manageJava").props.disabled, true);
  assert.deepEqual(navigation, []);
  assert.deepEqual(events, []);
  requests[1].reject(new Error("fixture cleanup"));
  await ui.flush();
  assert.equal(button(ui, "inspection.manageJava").props.disabled, false);
  ui.close();
});

test("saved repair handlers reject disablement, preview mode, and newer report generations", async () => {
  for (const blocked of [{ disabled: true }, { native: false }]) {
    const { ui, requests, calls } = await inspected();
    const oldRepair = button(ui, "inspection.repair").props.onClick;
    ui.replace(blocked);
    oldRepair();
    await ui.flush();
    assert.equal(calls.length, 1);
    ui.replace({ disabled: false, native: true });
    await ui.flush();
    assert.equal(calls.length, 2);
    requests[1].resolve(report());
    await ui.flush();
    oldRepair();
    assert.equal(calls.length, 2);
    button(ui, "inspection.repair").props.onClick();
    assert.equal(calls.length, 3);
    requests[2].reject(new Error("fixture cleanup"));
    await ui.flush();
    ui.close();
  }
  const { ui, calls, requests } = await inspected();
  const oldRepair = button(ui, "inspection.repair").props.onClick;
  button(ui, "inspection.recheck").props.onClick();
  oldRepair();
  assert.equal(calls.length, 2);
  requests[1].resolve(report("sample", { missing_total: 2 }));
  await new Promise((done) => setImmediate(done));
  oldRepair();
  assert.equal(calls.length, 2);
  await ui.flush();
  oldRepair();
  assert.equal(calls.length, 2);
  button(ui, "inspection.repair").props.onClick();
  assert.equal(calls.length, 3);
  requests[2].reject(new Error("fixture cleanup"));
  await ui.flush();
  ui.close();
});

test("repair failures remain visible and retryable while explicit recheck clears the error", async () => {
  const { ui, calls, requests, events } = await inspected();
  button(ui, "inspection.repair").props.onClick();
  requests[1].reject(new Error("Synthetic repair failed"));
  await ui.flush();
  assert.ok(content(ui.tree).includes(ui.i18n.t("inspection.repairFailed")));
  assert.ok(content(ui.tree).includes("Synthetic repair failed"));
  assert.equal(
    elements(ui.tree, (node) => node.props?.role === "alert").length,
    1,
  );
  assert.equal(button(ui, "inspection.repair").props.disabled, false);
  button(ui, "inspection.repair").props.onClick();
  await ui.flush();
  assert.ok(!content(ui.tree).includes("Synthetic repair failed"));
  requests[2].reject(new Error("Retry failed"));
  await ui.flush();
  button(ui, "inspection.recheck").props.onClick();
  await ui.flush();
  assert.ok(!content(ui.tree).includes("Retry failed"));
  requests[3].resolve(report());
  await ui.flush();
  assert.equal(calls.length, 4);
  assert.deepEqual(events, []);
  assert.equal(button(ui, "inspection.repair").props.disabled, false);
  ui.close();
});

test("retired repair failures stay silent after close, unmount, or a changed scope", async () => {
  for (const exit of ["close", "unmount", "root", "api"]) {
    const { ui, requests, events } = await inspected();
    button(ui, "inspection.repair").props.onClick();
    if (exit === "close") button(ui, "inspection.close").props.onClick();
    if (exit === "unmount") ui.close();
    if (exit === "root") ui.replace({ scopeKey: "root-b" });
    if (exit === "api")
      ui.replace({
        api: async () => report("sample", { game_dir: "current API" }),
      });
    requests[1].reject(new Error("Retired repair failure"));
    await ui.flush();
    assert.ok(!content(ui.tree).includes("Retired repair failure"));
    assert.deepEqual(events, exit === "close" ? [{ type: "close" }] : []);
    ui.close();
  }
});

test("native busy changes preserve repair ownership and never issue another inspection", async () => {
  for (const outcome of ["success", "failure"]) {
    const { ui, calls, requests, events } = await inspected();
    const recheck = button(ui, "inspection.recheck").props.onClick;
    button(ui, "inspection.repair").props.onClick();
    for (const blocked of [
      { disabled: true },
      { native: false },
      { disabled: false, native: true },
    ]) {
      ui.replace(blocked);
      recheck();
      await ui.flush();
      assert.equal(calls.length, 2);
      assert.equal(
        button(ui, "inspection.repairStarting").props.disabled,
        true,
      );
      assert.ok(content(ui.tree).includes("/synthetic/root/versions/sample"));
    }
    ui.replace({ disabled: true });
    if (outcome === "success") requests[1].resolve({ id: "busy-task" });
    else requests[1].reject(new Error("Busy repair failed"));
    await ui.flush();
    assert.equal(calls.length, 2);
    if (outcome === "success")
      assert.deepEqual(events, [
        { type: "close" },
        { type: "task", id: "busy-task", reveal: true },
      ]);
    else {
      assert.ok(content(ui.tree).includes("Busy repair failed"));
      ui.replace({ disabled: false });
      await ui.flush();
      assert.equal(calls.length, 2);
      assert.ok(content(ui.tree).includes("Busy repair failed"));
      assert.equal(button(ui, "inspection.repair").props.disabled, false);
      button(ui, "inspection.repair").props.onClick();
      requests[2].resolve({ id: "busy-retry-task" });
      await ui.flush();
      assert.deepEqual(events, [
        { type: "close" },
        { type: "task", id: "busy-retry-task", reveal: true },
      ]);
    }
    ui.close();
  }
});

test("closing during native busy submission leaves cancellation to the task manager", async () => {
  const { ui, calls, requests, events } = await inspected();
  button(ui, "inspection.repair").props.onClick();
  ui.replace({ disabled: true, native: false });
  const shade = elements(
    ui.tree,
    (node) => node.props?.className === "modal-shade ce-inspection-shade",
  )[0];
  shade.props.onMouseDown({ target: shade, currentTarget: shade });
  requests[1].resolve({ id: "closed-busy-task" });
  await ui.flush();
  assert.deepEqual(events, [
    { type: "close" },
    { type: "task", id: "closed-busy-task", reveal: false },
  ]);
  assert.deepEqual(
    calls.map((call) => call.command),
    ["inspect_instance", "instance_repair_start"],
  );
  ui.close();
});
