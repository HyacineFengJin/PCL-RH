/** Real failure-dialog hooks and handlers with synthetic native reports.
 * No game, Java, account, credential or actual game-root/log access is used. */
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  harness,
  elements,
  content,
  button,
  deferred,
} from "./maker-test-harness.mjs";

function status(extra = {}) {
  return {
    stage: "failed",
    message: "Synthetic launch failed.",
    version: "sample",
    pid: null,
    exit_code: 1,
    root_id: "synthetic-root-a",
    root_path: "/synthetic/root-a",
    ...extra,
  };
}
function report(extra = {}) {
  return {
    version: "sample",
    exit_code: 1,
    message: "Synthetic launch failed.",
    findings: [
      { code: "java_version", evidence: ["UnsupportedClassVersionError"] },
    ],
    log_name: "synthetic-launch.log",
    log_tail: "Synthetic line one\nSynthetic line two",
    log_error: null,
    log_truncated: false,
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
  const ui = harness("LaunchFailure", {
    expected: status(),
    scopeKey: "synthetic-root-a",
    api,
    native: true,
    disabled: false,
    canInspect: true,
    onInspect() {
      navigation.push({ kind: "inspect" });
    },
    onLogs(name) {
      navigation.push({ kind: "logs", name });
    },
    onClose() {
      closes++;
    },
    ...extra,
  });
  return {
    ui,
    calls,
    requests,
    navigation,
    get closes() {
      return closes;
    },
  };
}
function modal(ui) {
  return elements(ui.tree, (node) => node.props?.role === "dialog")[0];
}
function shade(ui) {
  return elements(
    ui.tree,
    (node) => node.props?.className === "modal-shade ce-failure-shade",
  )[0];
}
function noButton(ui, key) {
  assert.equal(
    elements(
      ui.tree,
      (node) => node.type === "button" && content(node) === ui.i18n.t(key),
    ).length,
    0,
  );
}

test("mount requests the exact failed-run snapshot once and repeated refresh cannot duplicate the read", async () => {
  const { ui, calls, requests } = dialog();
  await ui.flush();
  assert.deepEqual(calls, [
    { command: "launch_failure_report", args: { expected: status() } },
  ]);
  assert.equal(modal(ui).props["aria-busy"], true);
  assert.ok(content(ui.tree).includes("Synthetic launch failed."));
  assert.equal(button(ui, "failure.refresh").props.disabled, true);
  button(ui, "failure.refresh").props.onClick();
  button(ui, "failure.refresh").props.onClick();
  ui.replace({ expected: { ...status() } });
  await ui.flush();
  assert.equal(calls.length, 1);
  requests[0].resolve(report());
  await ui.flush();
  assert.equal(modal(ui).props["aria-busy"], false);
  assert.equal(button(ui, "failure.refresh").props.disabled, false);
  ui.close();
});

test("all clue categories use suggestions, with folded evidence and folded wrapping log text", async () => {
  const { ui, requests } = dialog();
  const codes = [
    "java_runtime",
    "java_version",
    "java_memory",
    "java_option",
    "missing_dependency",
    "graphics",
    "mod_loading",
    "unknown",
  ];
  requests[0].resolve(
    report({
      findings: codes.map((code) => ({
        code,
        evidence: [`Synthetic ${code} evidence`],
      })),
      log_truncated: true,
      argv: ["private argument"],
      access_token: "private token",
    }),
  );
  await ui.flush();
  const text = content(ui.tree);
  for (const code of codes) {
    assert.ok(text.includes(ui.i18n.t(`failure.${code}.title`)));
    assert.ok(text.includes(ui.i18n.t(`failure.${code}.suggestion`)));
    assert.ok(text.includes(`Synthetic ${code} evidence`));
  }
  assert.ok(text.includes(ui.i18n.t("failure.help")));
  assert.ok(text.includes(ui.i18n.t("failure.logTruncated")));
  assert.ok(text.includes("synthetic-launch.log"));
  assert.ok(!text.includes("private argument"));
  assert.ok(!text.includes("private token"));
  const details = elements(ui.tree, (node) => node.type === "details");
  assert.equal(details.length, codes.length + 1);
  for (const detail of details) assert.equal(detail.props.open, undefined);
  assert.equal(
    elements(ui.tree, (node) => node.type === "pre")[0].props.children,
    "Synthetic line one\nSynthetic line two",
  );
  ui.close();
});

test("empty and future clue codes show the unknown suggestion without losing evidence", async () => {
  for (const findings of [
    [],
    [{ code: "future_clue", evidence: ["New synthetic clue"] }],
  ]) {
    const { ui, requests } = dialog();
    requests[0].resolve(report({ findings }));
    await ui.flush();
    assert.ok(content(ui.tree).includes(ui.i18n.t("failure.unknown.title")));
    assert.ok(
      content(ui.tree).includes(ui.i18n.t("failure.unknown.suggestion")),
    );
    if (findings.length)
      assert.ok(content(ui.tree).includes("New synthetic clue"));
    ui.close();
  }
});

test("a failure before process startup handles nullable metadata and preserves a zero exit code", async () => {
  for (const exit_code of [null, 0]) {
    const { ui, requests } = dialog({
      expected: status({ version: null, exit_code }),
    });
    requests[0].resolve(
      report({
        version: null,
        exit_code,
        log_name: null,
        log_tail: null,
        findings: [],
      }),
    );
    await ui.flush();
    const text = content(ui.tree);
    assert.ok(text.includes("Synthetic launch failed."));
    assert.ok(!text.includes("null"));
    assert.ok(!text.includes("/synthetic/root-a"));
    if (exit_code === null)
      assert.ok(!text.includes(ui.i18n.t("failure.exitCode")));
    else assert.ok(text.includes(`${ui.i18n.t("failure.exitCode")}0`));
    assert.equal(button(ui, "failure.inspect").props.disabled, false);
    ui.close();
  }
});

test("no log and log-read failure retain the launch message and environment-check action", async () => {
  for (const log_error of [null, "Synthetic log permission failure"]) {
    const fixture = dialog(),
      { ui, requests, navigation } = fixture;
    requests[0].resolve(
      report({ log_name: null, log_tail: null, log_error, findings: [] }),
    );
    await ui.flush();
    assert.ok(content(ui.tree).includes("Synthetic launch failed."));
    assert.ok(
      content(ui.tree).includes(
        ui.i18n.t(log_error ? "failure.logReadFailed" : "failure.noLog"),
      ),
    );
    if (log_error) assert.ok(content(ui.tree).includes(log_error));
    noButton(ui, "failure.viewLogs");
    assert.equal(button(ui, "failure.inspect").props.disabled, false);
    button(ui, "failure.inspect").props.onClick();
    button(ui, "failure.inspect").props.onClick();
    assert.deepEqual(navigation, [{ kind: "inspect" }]);
    assert.equal(fixture.closes, 1);
    ui.close();
  }
});

test("an empty or unreadable named log can still open its existing logs-page export action", async () => {
  for (const log_error of [null, "Synthetic log failure"]) {
    const fixture = dialog(),
      { ui, requests, navigation } = fixture;
    requests[0].resolve(report({ log_tail: "", log_error }));
    await ui.flush();
    assert.ok(!content(ui.tree).includes(ui.i18n.t("failure.noLog")));
    if (!log_error)
      assert.ok(content(ui.tree).includes(ui.i18n.t("failure.emptyLog")));
    button(ui, "failure.viewLogs").props.onClick();
    button(ui, "failure.viewLogs").props.onClick();
    assert.deepEqual(navigation, [
      { kind: "logs", name: "synthetic-launch.log" },
    ]);
    assert.equal(fixture.closes, 1);
    ui.close();
  }
});

test("request errors and mismatched replies remain retryable and keep the failed-run message", async () => {
  const { ui, requests, calls } = dialog();
  requests[0].reject(new Error("Synthetic report failure"));
  await ui.flush();
  assert.ok(content(ui.tree).includes("Synthetic report failure"));
  assert.ok(content(ui.tree).includes("Synthetic launch failed."));
  assert.equal(
    elements(ui.tree, (node) => node.props?.role === "alert").length,
    1,
  );
  assert.equal(button(ui, "failure.inspect").props.disabled, false);
  for (const wrong of [{ version: "other" }, { exit_code: 2 }]) {
    button(ui, "failure.refresh").props.onClick();
    requests.at(-1).resolve(report(wrong));
    await ui.flush();
    assert.ok(content(ui.tree).includes(ui.i18n.t("failure.wrongRun")));
  }
  button(ui, "failure.refresh").props.onClick();
  requests.at(-1).resolve(report());
  await ui.flush();
  assert.equal(calls.length, 4);
  assert.equal(
    elements(ui.tree, (node) => node.props?.role === "alert").length,
    0,
  );
  ui.close();
});

test("a changed expected status retires pending replies and every saved run action", async () => {
  const fixture = dialog(),
    { ui, requests, calls, navigation } = fixture;
  const oldClose = button(ui, "failure.close").props.onClick;
  const oldRefresh = button(ui, "failure.refresh").props.onClick;
  const oldInspect = button(ui, "failure.inspect").props.onClick;
  ui.replace({
    expected: status({ message: "A second launch failed.", exit_code: 2 }),
  });
  oldClose();
  oldRefresh();
  oldInspect();
  requests[0].resolve(report({ message: "Retired first report" }));
  await ui.flush();
  assert.equal(calls.length, 2);
  assert.equal(fixture.closes, 0);
  assert.deepEqual(navigation, []);
  assert.ok(!content(ui.tree).includes("Retired first report"));
  assert.ok(content(ui.tree).includes("A second launch failed."));
  requests[1].resolve(
    report({ message: "Current second report", exit_code: 2 }),
  );
  await ui.flush();
  assert.ok(content(ui.tree).includes("Current second report"));
  ui.close();
});

test("each captured status field changes request ownership, including root identity and path", async () => {
  for (const change of [
    { stage: "other-stage" },
    { message: "Other message" },
    { version: "other" },
    { pid: 42 },
    { exit_code: 2 },
    { root_id: "synthetic-root-b" },
    { root_path: "/synthetic/root-b" },
  ]) {
    const { ui, calls, requests } = dialog();
    const next = status(change);
    ui.replace({ expected: next });
    await ui.flush();
    assert.equal(calls.length, 2);
    assert.deepEqual(calls[1].args.expected, next);
    requests[0].resolve(report({ message: "Retired status report" }));
    requests[1].resolve(
      report({ version: next.version, exit_code: next.exit_code }),
    );
    await ui.flush();
    assert.ok(!content(ui.tree).includes("Retired status report"));
    ui.close();
  }
});

test("root scope and API replacement retire earlier successes and failures", async () => {
  const fixture = dialog(),
    { ui, requests, calls, navigation } = fixture;
  requests[0].resolve(report());
  await ui.flush();
  const oldLogs = button(ui, "failure.viewLogs").props.onClick;
  ui.replace({ scopeKey: "synthetic-root-b" });
  oldLogs();
  await ui.flush();
  assert.equal(calls.length, 2);
  const replacement = deferred();
  let replacementReads = 0;
  ui.replace({
    api: async () => {
      replacementReads++;
      return replacement.promise;
    },
  });
  requests[1].reject(new Error("Retired root failure"));
  replacement.resolve(report({ message: "Replacement API report" }));
  await ui.flush();
  assert.equal(replacementReads, 1);
  assert.deepEqual(navigation, []);
  assert.equal(fixture.closes, 0);
  assert.ok(content(ui.tree).includes("Replacement API report"));
  assert.ok(!content(ui.tree).includes("Retired root failure"));
  ui.close();
});

test("an old API success arriving last cannot overwrite its replacement", async () => {
  const { ui, requests } = dialog(),
    replacement = deferred();
  ui.replace({ api: async () => replacement.promise });
  replacement.resolve(report({ message: "Current API report" }));
  await ui.flush();
  requests[0].resolve(report({ message: "Retired API report" }));
  await ui.flush();
  assert.ok(content(ui.tree).includes("Current API report"));
  assert.ok(!content(ui.tree).includes("Retired API report"));
  ui.close();
});

test("preview and disabled scopes do not read or navigate; enabling starts a fresh request", async () => {
  for (const extra of [{ native: false }, { disabled: true }]) {
    const fixture = dialog(extra),
      { ui, calls, navigation } = fixture;
    await ui.flush();
    assert.equal(calls.length, 0);
    assert.equal(button(ui, "failure.refresh").props.disabled, true);
    assert.equal(button(ui, "failure.inspect").props.disabled, true);
    button(ui, "failure.refresh").props.onClick();
    button(ui, "failure.inspect").props.onClick();
    assert.equal(calls.length, 0);
    assert.deepEqual(navigation, []);
    assert.ok(
      content(ui.tree).includes(
        ui.i18n.t(
          extra.native === false ? "failure.desktopOnly" : "failure.disabled",
        ),
      ),
    );
    ui.replace({ native: true, disabled: false });
    await ui.flush();
    assert.equal(calls.length, 1);
    ui.close();
  }
});

test("disabling and enabling retires saved log actions and an in-flight report permanently", async () => {
  const fixture = dialog(),
    { ui, requests, calls, navigation } = fixture;
  requests[0].resolve(report());
  await ui.flush();
  const oldLogs = button(ui, "failure.viewLogs").props.onClick;
  const oldInspect = button(ui, "failure.inspect").props.onClick;
  const oldClose = button(ui, "failure.close").props.onClick;
  button(ui, "failure.refresh").props.onClick();
  ui.replace({ disabled: true });
  oldLogs();
  oldInspect();
  oldClose();
  ui.replace({ disabled: false });
  oldLogs();
  oldInspect();
  oldClose();
  requests[1].resolve(report({ message: "Disabled report" }));
  await ui.flush();
  assert.equal(calls.length, 3);
  assert.deepEqual(navigation, []);
  assert.equal(fixture.closes, 0);
  assert.ok(!content(ui.tree).includes("Disabled report"));
  requests[2].resolve(report());
  await ui.flush();
  oldLogs();
  assert.deepEqual(navigation, []);
  button(ui, "failure.viewLogs").props.onClick();
  assert.deepEqual(navigation, [
    { kind: "logs", name: "synthetic-launch.log" },
  ]);
  ui.close();
});

test("environment inspection respects current availability and retires a pending report", async () => {
  const fixture = dialog(),
    { ui, calls, requests, navigation } = fixture;
  const inspect = button(ui, "failure.inspect").props.onClick;
  ui.replace({ canInspect: false });
  assert.equal(button(ui, "failure.inspect").props.disabled, true);
  inspect();
  assert.deepEqual(navigation, []);
  ui.replace({ canInspect: true });
  assert.equal(calls.length, 1);
  button(ui, "failure.inspect").props.onClick();
  requests[0].resolve(report({ message: "Navigated-away report" }));
  await ui.flush();
  assert.deepEqual(navigation, [{ kind: "inspect" }]);
  assert.equal(fixture.closes, 1);
  assert.ok(!content(ui.tree).includes("Navigated-away report"));
  ui.close();
});

test("refresh immediately retires old log actions, including before the next React render", async () => {
  const { ui, requests, navigation } = dialog();
  requests[0].resolve(report());
  await ui.flush();
  const oldLogs = button(ui, "failure.viewLogs").props.onClick;
  button(ui, "failure.refresh").props.onClick();
  oldLogs();
  requests[1].resolve(report({ log_name: "new-synthetic-launch.log" }));
  await new Promise((done) => setImmediate(done));
  oldLogs();
  assert.deepEqual(navigation, []);
  await ui.flush();
  oldLogs();
  assert.deepEqual(navigation, []);
  button(ui, "failure.viewLogs").props.onClick();
  assert.deepEqual(navigation, [
    { kind: "logs", name: "new-synthetic-launch.log" },
  ]);
  ui.close();
});

test("close is immediate and idempotent; unmount retires pending replies and handlers", async () => {
  for (const mode of ["close", "unmount"]) {
    const fixture = dialog(),
      { ui, calls, requests, navigation } = fixture;
    const close = button(ui, "failure.close").props.onClick;
    const refresh = button(ui, "failure.refresh").props.onClick;
    const inspect = button(ui, "failure.inspect").props.onClick;
    if (mode === "unmount") ui.close();
    close();
    close();
    refresh();
    inspect();
    requests[0].reject(new Error("Retired report failure"));
    await ui.flush();
    assert.equal(fixture.closes, mode === "close" ? 1 : 0);
    assert.equal(calls.length, 1);
    assert.deepEqual(navigation, []);
    assert.ok(!content(ui.tree).includes("Retired report failure"));
    ui.close();
  }
});

test("Escape and backdrop close retire replies while clicks inside keep the modal", async () => {
  for (const mode of ["escape", "backdrop"]) {
    const fixture = dialog(),
      { ui, requests } = fixture;
    const backdrop = shade(ui);
    backdrop.props.onMouseDown({ target: {}, currentTarget: backdrop });
    assert.equal(fixture.closes, 0);
    if (mode === "escape")
      modal(ui).props.onKeyDown({
        key: "Escape",
        preventDefault() {},
        stopPropagation() {},
      });
    else
      backdrop.props.onMouseDown({ target: backdrop, currentTarget: backdrop });
    requests[0].resolve(report({ message: "Closed report" }));
    await ui.flush();
    assert.equal(fixture.closes, 1);
    assert.ok(!content(ui.tree).includes("Closed report"));
    ui.close();
  }
});

test("Tab and Shift-Tab bring outside focus into the dialog, with a safe empty-control fallback", async () => {
  const { ui } = dialog();
  let focused = "",
    prevented = 0,
    stopped = 0;
  const first = {
      focus() {
        focused = "first";
      },
    },
    last = {
      focus() {
        focused = "last";
      },
    };
  for (const shiftKey of [false, true]) {
    modal(ui).props.onKeyDown({
      key: "Tab",
      shiftKey,
      currentTarget: {
        querySelectorAll() {
          return [first, last];
        },
      },
      preventDefault() {
        prevented++;
      },
      stopPropagation() {
        stopped++;
      },
    });
    assert.equal(focused, shiftKey ? "last" : "first");
  }
  modal(ui).props.onKeyDown({
    key: "Tab",
    shiftKey: false,
    currentTarget: {
      querySelectorAll() {
        return [];
      },
      focus() {
        focused = "modal";
      },
    },
    preventDefault() {
      prevented++;
    },
    stopPropagation() {
      stopped++;
    },
  });
  assert.equal(focused, "modal");
  assert.equal(prevented, 3);
  assert.equal(stopped, 3);
  assert.equal(modal(ui).props.tabIndex, -1);
  ui.close();
});

test("Chinese and English failure strings have matching keys and the English dialog has translated suggestions", async () => {
  const { ui, requests } = dialog();
  const english = ui.i18n.createTranslator({
    language: "en-US",
    region: "en-US",
  });
  const chinese = ui.i18n.createTranslator({
    language: "zh-CN",
    region: "zh-CN",
  });
  const keys = [
    "open",
    "help",
    "loading",
    "desktopOnly",
    "disabled",
    "failed",
    "wrongRun",
    "refresh",
    "close",
    "inspect",
    "viewLogs",
    "version",
    "exitCode",
    "evidence",
    "logTail",
    "logTruncated",
    "logReadFailed",
    "noLog",
    "emptyLog",
  ];
  for (const code of [
    "java_runtime",
    "java_version",
    "java_memory",
    "java_option",
    "missing_dependency",
    "graphics",
    "mod_loading",
    "unknown",
  ])
    keys.push(`${code}.title`, `${code}.suggestion`);
  for (const suffix of keys) {
    const key = `failure.${suffix}`;
    assert.notEqual(chinese.t(key), key);
    assert.notEqual(english.t(key), key);
  }
  ui.i18n.t = english.t;
  ui.i18n.serviceError = english.serviceError;
  ui.replace({});
  requests[0].resolve(report());
  await ui.flush();
  for (const key of [
    "failure.open",
    "failure.java_version.title",
    "failure.java_version.suggestion",
    "failure.help",
  ])
    assert.ok(content(ui.tree).includes(english.t(key)));
  button(ui, "failure.inspect");
  button(ui, "failure.viewLogs");
  ui.close();
});
