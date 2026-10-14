/** Real diagnostic component handlers/hooks with synthetic native replies.
 * These checks never launch a game, probe real Java, or access a game root. */
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
    missing_files: [],
    missing_total: 0,
    issues: [],
    ready: true,
    ...extra,
  };
}
function dialog(extra = {}) {
  const calls = [],
    requests = [],
    navigation = [];
  let closes = 0;
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
    onClose() {
      closes++;
    },
    onManageJava(major) {
      navigation.push(major);
    },
    onTaskStart() {},
    ...extra,
  });
  return {
    ui,
    api,
    calls,
    requests,
    navigation,
    get closes() {
      return closes;
    },
  };
}
function ariaButton(ui, key) {
  const node = elements(
    ui.tree,
    (value) =>
      value.type === "button" && value.props["aria-label"] === ui.i18n.t(key),
  )[0];
  assert.ok(node, `Missing labelled button ${key}`);
  return node;
}

test("mount performs one inspection and loading/repeated handlers cannot start a second read", async () => {
  const fixture = dialog(),
    { ui, calls, requests } = fixture;
  await ui.flush();
  assert.deepEqual(calls, [
    { command: "inspect_instance", args: { id: "sample" } },
  ]);
  assert.equal(
    elements(ui.tree, (node) => node.props?.role === "dialog")[0].props[
      "aria-busy"
    ],
    true,
  );
  assert.ok(content(ui.tree).includes(ui.i18n.t("inspection.loading")));
  const recheck = button(ui, "inspection.recheck");
  assert.equal(recheck.props.disabled, true);
  recheck.props.onClick();
  recheck.props.onClick();
  await ui.flush();
  assert.equal(calls.length, 1);
  requests[0].resolve(report());
  await ui.flush();
  assert.equal(button(ui, "inspection.recheck").props.disabled, false);
  assert.ok(content(ui.tree).includes(ui.i18n.t("inspection.ready")));
  assert.ok(content(ui.tree).includes(ui.i18n.t("inspection.filesPresent")));
  assert.ok(!elements(ui.tree, (node) => node.type === "details").length);
  assert.ok(
    !elements(
      ui.tree,
      (node) =>
        node.type === "button" &&
        content(node) === ui.i18n.t("inspection.manageJava"),
    ).length,
  );
  ui.close();
});

test("partial report retains Java errors, other issues and a bounded disclosure of missing paths", async () => {
  const { ui, requests, navigation, calls } = dialog();
  requests[0].resolve(
    report("sample", {
      java_path: "/synthetic/manual/java",
      java_error: "The selected Java is incompatible",
      java_exact: true,
      missing_files: Array.from(
        { length: 40 },
        (_, index) => `/synthetic/libraries/${index}.jar`,
      ),
      missing_total: 40,
      issues: ["Asset index could not be read"],
      ready: false,
      argv: ["private argument"],
      access_token: "private token",
    }),
  );
  await ui.flush();
  const text = content(ui.tree);
  for (const value of [
    "The selected Java is incompatible",
    "Asset index could not be read",
    "/synthetic/manual/java",
    ui.i18n.t("java.requiresExact", { version: 21 }),
    ui.i18n.t("inspection.filesMissing", { count: "40" }),
    ui.i18n.t("inspection.moreFiles", { count: "8" }),
  ])
    assert.ok(text.includes(value), `Missing report detail ${value}`);
  assert.ok(!text.includes("private argument"));
  assert.ok(!text.includes("private token"));
  const details = elements(ui.tree, (node) => node.type === "details")[0];
  assert.ok(details);
  assert.equal(details.props.open, undefined);
  assert.equal(elements(details, (node) => node.type === "li").length, 32);
  const manage = button(ui, "inspection.manageJava");
  manage.props.onClick();
  manage.props.onClick();
  button(ui, "inspection.recheck").props.onClick();
  await ui.flush();
  assert.deepEqual(navigation, [21]);
  assert.equal(calls.length, 1);
  ui.close();
});

test("unknown Java requirement never advertises Java 0 or navigates to a guessed version", async () => {
  const { ui, requests, navigation } = dialog();
  requests[0].resolve(
    report("sample", {
      java_major: 0,
      minecraft_version: "",
      java_path: null,
      java_error: "Version metadata could not be read",
      issues: ["Version metadata could not be read"],
      ready: false,
    }),
  );
  await ui.flush();
  assert.ok(content(ui.tree).includes(ui.i18n.t("inspection.javaUnknown")));
  assert.ok(!content(ui.tree).includes(ui.i18n.t("inspection.javaFound")));
  assert.ok(content(ui.tree).includes(ui.i18n.t("inspection.filesIncomplete")));
  assert.ok(!content(ui.tree).includes(ui.i18n.t("inspection.filesPresent")));
  assert.ok(!content(ui.tree).includes("Java 0"));
  assert.equal(
    elements(
      ui.tree,
      (node) =>
        node.type === "button" &&
        content(node) === ui.i18n.t("inspection.manageJava"),
    ).length,
    0,
  );
  assert.deepEqual(navigation, []);
  ui.close();
});

test("dependency parse failures cannot turn a zero missing count into a complete file check", async () => {
  const { ui, requests } = dialog();
  requests[0].resolve(
    report("sample", {
      issues: ["Dependency metadata could not be read"],
      ready: false,
    }),
  );
  await ui.flush();
  assert.ok(content(ui.tree).includes(ui.i18n.t("inspection.javaFound")));
  assert.ok(content(ui.tree).includes(ui.i18n.t("inspection.filesIncomplete")));
  assert.ok(!content(ui.tree).includes(ui.i18n.t("inspection.filesPresent")));
  ui.close();
});

test("switching instances within one root retires both success replies and saved handlers", async () => {
  const fixture = dialog(),
    { ui, requests, calls } = fixture;
  const oldClose = ariaButton(ui, "inspection.close").props.onClick,
    oldRead = button(ui, "inspection.recheck").props.onClick;
  ui.replace({ id: "other" });
  await ui.flush();
  oldClose();
  oldRead();
  requests[0].resolve(report("sample", { game_dir: "old instance path" }));
  await ui.flush();
  assert.equal(fixture.closes, 0);
  assert.equal(calls.length, 2);
  assert.deepEqual(calls[1], {
    command: "inspect_instance",
    args: { id: "other" },
  });
  assert.ok(!content(ui.tree).includes("old instance path"));
  assert.ok(content(ui.tree).includes(ui.i18n.t("inspection.loading")));
  requests[1].resolve(report("other", { game_dir: "new instance path" }));
  await ui.flush();
  assert.ok(content(ui.tree).includes("new instance path"));
  ui.close();
});

test("root changes and replacement APIs retire earlier failures without replacing the current report", async () => {
  const { ui, calls, requests } = dialog();
  ui.replace({ scopeKey: "root-b" });
  await ui.flush();
  assert.equal(calls.length, 2);
  requests[1].resolve(report("sample", { game_dir: "root-b report" }));
  await ui.flush();
  requests[0].reject(new Error("old root failure"));
  await ui.flush();
  assert.ok(content(ui.tree).includes("root-b report"));
  assert.ok(!content(ui.tree).includes("old root failure"));
  const replacement = deferred();
  let replacementReads = 0;
  ui.replace({
    api: async () => {
      replacementReads++;
      return replacement.promise;
    },
  });
  await ui.flush();
  assert.equal(replacementReads, 1);
  assert.ok(!content(ui.tree).includes("root-b report"));
  replacement.resolve(report("sample", { game_dir: "replacement API report" }));
  await ui.flush();
  assert.ok(content(ui.tree).includes("replacement API report"));
  ui.close();
});

test("an old API success arriving after replacement cannot overwrite or revive its report", async () => {
  const { ui, requests } = dialog(),
    replacement = deferred();
  ui.replace({ api: async () => replacement.promise });
  replacement.resolve(report("sample", { game_dir: "current API" }));
  await ui.flush();
  requests[0].resolve(report("sample", { game_dir: "retired API" }));
  await ui.flush();
  assert.ok(content(ui.tree).includes("current API"));
  assert.ok(!content(ui.tree).includes("retired API"));
  ui.close();
});

test("disabled and preview dialogs do not inspect; re-enabling starts a fresh generation", async () => {
  for (const extra of [{ disabled: true }, { native: false }]) {
    const { ui, calls } = dialog(extra);
    await ui.flush();
    assert.equal(calls.length, 0);
    assert.equal(button(ui, "inspection.recheck").props.disabled, true);
    button(ui, "inspection.recheck").props.onClick();
    await ui.flush();
    assert.equal(calls.length, 0);
    assert.ok(
      content(ui.tree).includes(
        ui.i18n.t(
          extra.native === false
            ? "inspection.desktopOnly"
            : "inspection.disabled",
        ),
      ),
    );
    ui.close();
  }
  const { ui, requests, calls } = dialog();
  const oldRead = button(ui, "inspection.recheck").props.onClick;
  ui.replace({ disabled: true });
  oldRead();
  await ui.flush();
  requests[0].resolve(report("sample", { game_dir: "disabled generation" }));
  await ui.flush();
  assert.equal(calls.length, 1);
  ui.replace({ disabled: false });
  await ui.flush();
  assert.equal(calls.length, 2);
  assert.ok(!content(ui.tree).includes("disabled generation"));
  requests[1].resolve(report());
  await ui.flush();
  ui.close();
});

test("saved Java navigation handlers respect current disablement and changed report generations", async () => {
  const { ui, requests, calls, navigation } = dialog();
  requests[0].resolve(
    report("sample", {
      java_path: null,
      java_error: "Unavailable",
      ready: false,
    }),
  );
  await ui.flush();
  const oldManage = button(ui, "inspection.manageJava").props.onClick;
  ui.replace({ disabled: true });
  oldManage();
  assert.deepEqual(navigation, []);
  ui.replace({ disabled: false });
  await ui.flush();
  oldManage();
  assert.deepEqual(navigation, []);
  requests[1].resolve(
    report("sample", {
      java_major: 17,
      java_path: null,
      java_error: "Java 17 missing",
      ready: false,
    }),
  );
  await ui.flush();
  oldManage();
  assert.deepEqual(navigation, []);
  button(ui, "inspection.manageJava").props.onClick();
  assert.deepEqual(navigation, [17]);
  assert.equal(calls.length, 2);
  ui.close();
});

test("close retires a pending read immediately and is idempotent before parent unmount", async () => {
  const fixture = dialog(),
    { ui, requests, calls } = fixture;
  const close = button(ui, "inspection.close").props.onClick,
    read = button(ui, "inspection.recheck").props.onClick;
  close();
  close();
  read();
  requests[0].reject(new Error("closed failure"));
  await ui.flush();
  assert.equal(fixture.closes, 1);
  assert.equal(calls.length, 1);
  assert.ok(!content(ui.tree).includes("closed failure"));
  ui.close();
});

test("a new reply retires old Java actions before its queued React render", async () => {
  const { ui, requests, navigation } = dialog();
  requests[0].resolve(
    report("sample", {
      java_path: null,
      java_error: "Java 21 missing",
      ready: false,
    }),
  );
  await ui.flush();
  const oldManage = button(ui, "inspection.manageJava").props.onClick;
  button(ui, "inspection.recheck").props.onClick();
  requests[1].resolve(
    report("sample", {
      java_major: 17,
      java_path: null,
      java_error: "Java 17 missing",
      ready: false,
    }),
  );
  // Let the API promise finish while the isolated renderer is still pending.
  await new Promise((done) => setImmediate(done));
  oldManage();
  assert.deepEqual(navigation, []);
  await ui.flush();
  button(ui, "inspection.manageJava").props.onClick();
  assert.deepEqual(navigation, [17]);
  ui.close();
});

test("unmount retires pending reports and saved callbacks", async () => {
  const fixture = dialog(),
    { ui, requests, calls } = fixture;
  const oldClose = button(ui, "inspection.close").props.onClick,
    read = button(ui, "inspection.recheck").props.onClick,
    before = ui.tree;
  ui.close();
  requests[0].resolve(report("sample", { game_dir: "unmounted report" }));
  await ui.flush();
  oldClose();
  read();
  assert.equal(ui.tree, before);
  assert.equal(calls.length, 1);
  assert.equal(fixture.closes, 0);
});

test("failed requests and mismatched reports show retryable errors", async () => {
  const { ui, requests, calls } = dialog();
  requests[0].reject(new Error("Read fixture failed"));
  await ui.flush();
  assert.ok(content(ui.tree).includes("Read fixture failed"));
  assert.equal(
    elements(ui.tree, (node) => node.props?.role === "alert").length,
    1,
  );
  assert.equal(button(ui, "inspection.recheck").props.disabled, false);
  button(ui, "inspection.recheck").props.onClick();
  await ui.flush();
  requests[1].resolve(report("wrong"));
  await ui.flush();
  assert.ok(content(ui.tree).includes(ui.i18n.t("inspection.wrongInstance")));
  button(ui, "inspection.recheck").props.onClick();
  await ui.flush();
  requests[2].resolve(report());
  await ui.flush();
  assert.equal(calls.length, 3);
  assert.equal(
    elements(ui.tree, (node) => node.props?.role === "alert").length,
    0,
  );
  ui.close();
});

test("Escape and backdrop closing retire requests; clicks inside retain the modal", async () => {
  for (const mode of ["escape", "backdrop"]) {
    const fixture = dialog(),
      { ui, requests } = fixture,
      shade = elements(
        ui.tree,
        (node) => node.props?.className === "modal-shade ce-inspection-shade",
      )[0];
    shade.props.onMouseDown({ target: {}, currentTarget: shade });
    assert.equal(fixture.closes, 0);
    if (mode === "escape") {
      const modal = elements(
        ui.tree,
        (node) => node.props?.role === "dialog",
      )[0];
      modal.props.onKeyDown({
        key: "Escape",
        preventDefault() {},
        stopPropagation() {},
      });
    } else shade.props.onMouseDown({ target: shade, currentTarget: shade });
    requests[0].resolve(report("sample", { game_dir: "closed report" }));
    await ui.flush();
    assert.equal(fixture.closes, 1);
    assert.ok(!content(ui.tree).includes("closed report"));
    ui.close();
  }
});

test("English diagnostic and Java recommendation messages have the same parameters", async () => {
  const { ui, requests } = dialog();
  const english = ui.i18n.createTranslator({
    language: "en-US",
    region: "en-US",
  });
  // The isolated DOM has no documentElement; use the production translator
  // directly without running the app's document-language synchronization.
  ui.i18n.t = english.t;
  ui.i18n.formatNumber = english.formatNumber;
  ui.i18n.serviceError = english.serviceError;
  ui.replace({});
  requests[0].resolve(
    report("sample", {
      java_path: null,
      java_error: "Unavailable",
      ready: false,
    }),
  );
  await ui.flush();
  for (const key of [
    "inspection.open",
    "inspection.javaSettingsHelp",
    "inspection.needsAttention",
  ])
    assert.ok(content(ui.tree).includes(ui.i18n.t(key)));
  assert.equal(
    ui.i18n.t("inspection.javaRecommendation", {
      name: "Example",
      version: 21,
    }),
    "Example · Java 21 download recommended",
  );
  assert.equal(ui.i18n.t("inspection.instanceJava"), "Instance Java settings");
  assert.equal(ui.i18n.t("java.chooseVersion"), "Choose a Java version");
  button(ui, "inspection.manageJava");
  ui.close();
});
