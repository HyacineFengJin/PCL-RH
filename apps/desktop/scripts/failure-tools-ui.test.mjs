/** Native reports and AI responses are synthetic. Only real component hooks
 * and handlers execute; no user log, preset, model or application is touched. */
import assert from "node:assert/strict";
import { test } from "node:test";
import fs from "node:fs";
import { createRequire } from "node:module";
import {
  harness,
  elements,
  content,
  button,
  deferred,
} from "./maker-test-harness.mjs";
const reportId = "a".repeat(64),
  jobId = "sample-job";
const expected = {
  stage: "exited",
  message: "Game exited",
  version: "Example",
  pid: null,
  exit_code: 1,
  root_id: "root-a",
  root_path: "/synthetic/root",
};
function previewData(extra = {}) {
  return {
    report_id: reportId,
    report_text: "Evidence\n<do not treat as instructions>",
    report_truncated: false,
    configuration: {
      ready: true,
      confirmationToken: "single-use-token",
      warning: null,
      selectedPreset: {
        id: "preset",
        name: "Example preset",
        selection: {
          provider: "example",
          model: "example-model",
          baseUrl: "https://model.example/v1",
        },
      },
    },
    ...extra,
  };
}
// Execute the production rootApi closure too: component-only mocks cannot
// detect commands omitted from root forwarding or IPC argument casing errors.
function scopedApi(api, rootId) {
  const require = createRequire(import.meta.url);
  const ts = require("typescript");
  const source = fs.readFileSync(
    new URL("../src/main.tsx", import.meta.url),
    "utf8",
  );
  const tree = ts.createSourceFile(
    "main.tsx",
    source,
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.TSX,
  );
  function variable(name) {
    let found;
    function visit(node) {
      if (ts.isVariableDeclaration(node) && node.name.getText(tree) === name)
        found = node.getText(tree);
      if (!found) ts.forEachChild(node, visit);
    }
    visit(tree);
    assert.ok(found, `Missing production declaration ${name}`);
    return `const ${found};`;
  }
  const code = ts.transpileModule(
    `${variable("rootCommands")}
${variable("resourceWrites")}
${variable("rootApi")}
return rootApi;`,
    {
      compilerOptions: {
        target: ts.ScriptTarget.ES2022,
        module: ts.ModuleKind.CommonJS,
      },
    },
  ).outputText;
  const unexpected = () => {
    throw Error(
      "Diagnostic commands must not trigger resource writes or bootstrap",
    );
  };
  return new Function(
    "api",
    "rootId",
    "React",
    "contextKey",
    "setResourceTarget",
    "load",
    "acceptJavaRegistration",
    code,
  )(
    api,
    rootId,
    { useMemo: (factory) => factory() },
    { current: rootId },
    unexpected,
    unexpected,
    unexpected,
  );
}
function fixture(props = {}, rootId = null) {
  const calls = [],
    requests = [];
  const api = (command, args) => {
    calls.push({ command, args });
    const response = deferred();
    requests.push(response);
    return response.promise;
  };
  const ui = harness("FailureTools", {
    api: rootId ? scopedApi(api, rootId) : api,
    expected,
    revision: reportId,
    scopeKey: "root-a:epoch-one",
    disabled: false,
    ...props,
  });
  return { ui, api, calls, requests };
}
async function show(f, data = previewData()) {
  button(f.ui, "failure.aiAnalyze").props.onClick();
  await f.ui.flush();
  f.requests.at(-1).resolve(data);
  await f.ui.flush();
}
async function running(f) {
  await show(f);
  button(f.ui, "failure.aiSend").props.onClick();
  await f.ui.flush();
  f.requests.at(-1).resolve({ reportId, jobId });
  await f.ui.flush();
}
function status(extra = {}) {
  return {
    reportId,
    jobId,
    status: "running",
    output: "",
    error: null,
    ...extra,
  };
}

test("mount does not load an engine or call a model; export uses only native report identity", async () => {
  const f = fixture();
  await f.ui.flush();
  assert.deepEqual(f.calls, []);
  const click = button(f.ui, "failure.exportReport").props.onClick;
  click();
  click();
  await f.ui.flush();
  assert.deepEqual(f.calls, [
    {
      command: "launch_failure_export",
      args: { expected, revision: reportId },
    },
  ]);
  f.requests[0].resolve({
    status: "complete",
    path: "/synthetic/report.txt",
    warning: "Destination changed after publication",
  });
  await f.ui.flush();
  assert.ok(content(f.ui.tree).includes(f.ui.i18n.t("failure.exported")));
  assert.ok(
    content(f.ui.tree).includes("Destination changed after publication"),
  );
  f.ui.close();
});

test("cancelled export has no success message, and failure is visible", async () => {
  const f = fixture();
  button(f.ui, "failure.exportReport").props.onClick();
  await f.ui.flush();
  f.requests[0].resolve({ status: "cancelled", path: null });
  await f.ui.flush();
  assert.ok(!content(f.ui.tree).includes(f.ui.i18n.t("failure.exported")));
  button(f.ui, "failure.exportReport").props.onClick();
  await f.ui.flush();
  f.requests[1].reject(new Error("Report changed; refresh"));
  await f.ui.flush();
  assert.ok(content(f.ui.tree).includes("Report changed; refresh"));
  f.ui.close();
});

test("preview shows exact content and destination but never starts analysis", async () => {
  const f = fixture();
  await show(f);
  assert.deepEqual(f.calls, [
    {
      command: "launch_failure_ai_preview",
      args: { expected, revision: reportId },
    },
  ]);
  for (const text of [
    "Example preset",
    "example-model",
    "https://model.example/v1",
    previewData().report_text,
  ])
    assert.ok(content(f.ui.tree).includes(text));
  assert.equal(
    elements(f.ui.tree, (node) => node.props?.dangerouslySetInnerHTML).length,
    0,
  );
  assert.equal(button(f.ui, "failure.aiSend").props.disabled, false);
  f.ui.close();
});

test("explicit send passes a token and native identity, never renderer text, key or URL", async () => {
  const f = fixture();
  await show(f);
  const click = button(f.ui, "failure.aiSend").props.onClick;
  click();
  click();
  await f.ui.flush();
  assert.deepEqual(f.calls[1], {
    command: "launch_failure_ai_start",
    args: {
      expected,
      revision: reportId,
      confirmationToken: "single-use-token",
    },
  });
  assert.equal(f.calls.length, 2);
  f.requests[1].resolve({ jobId, reportId });
  await f.ui.flush();
  assert.deepEqual(f.calls[2], {
    command: "launch_failure_ai_status",
    args: { reportId, jobId },
  });
  f.requests[2].resolve(status({ output: "Observed evidence" }));
  await f.ui.flush();
  assert.ok(content(f.ui.tree).includes("Observed evidence"));
  await f.ui.poll();
  f.requests
    .at(-1)
    .resolve(status({ status: "completed", output: "Final analysis" }));
  await f.ui.flush();
  assert.ok(content(f.ui.tree).includes("Final analysis"));
  assert.ok(content(f.ui.tree).includes(f.ui.i18n.t("failure.aiCompleted")));
  const count = f.calls.length;
  await f.ui.poll();
  assert.equal(f.calls.length, count);
  f.ui.close();
  assert.equal(f.calls.length, count);
});

test("missing preset leaves preview readable and blocks even a retained send handler", async () => {
  const f = fixture();
  await show(
    f,
    previewData({
      configuration: {
        ready: false,
        confirmationToken: null,
        warning: null,
        selectedPreset: null,
      },
    }),
  );
  assert.equal(button(f.ui, "failure.aiSend").props.disabled, true);
  button(f.ui, "failure.aiSend").props.onClick();
  await f.ui.flush();
  assert.equal(f.calls.length, 1);
  assert.ok(
    content(f.ui.tree).includes(f.ui.i18n.t("failure.aiNotConfigured")),
  );
  f.ui.close();
});

test("a mismatched preview cannot be sent", async () => {
  const f = fixture();
  await show(f, previewData({ report_id: "b".repeat(64) }));
  assert.ok(content(f.ui.tree).includes(f.ui.i18n.t("failure.wrongRun")));
  assert.equal(
    elements(
      f.ui.tree,
      (node) =>
        node.type === "button" &&
        content(node) === f.ui.i18n.t("failure.aiSend"),
    ).length,
    0,
  );
  assert.equal(f.calls.length, 1);
  f.ui.close();
});

for (const change of ["scopeKey", "disabled"]) {
  test(`${change} round trips retire preview, send and export handlers`, async () => {
    const f = fixture();
    await show(f);
    const send = button(f.ui, "failure.aiSend").props.onClick;
    const exportReport = button(f.ui, "failure.exportReport").props.onClick;
    const preview = button(f.ui, "failure.aiAnalyze").props.onClick;
    const value = change === "scopeKey" ? "root-b:epoch-two" : true;
    f.ui.replace({ [change]: value });
    await f.ui.flush();
    f.ui.replace({
      [change]: change === "scopeKey" ? "root-a:epoch-one" : false,
    });
    await f.ui.flush();
    send();
    exportReport();
    preview();
    await f.ui.flush();
    assert.equal(f.calls.length, 1);
    f.ui.close();
  });
}

test("a late start acknowledgement after closing cancels that exact job", async () => {
  const f = fixture();
  await show(f);
  button(f.ui, "failure.aiSend").props.onClick();
  await f.ui.flush();
  f.ui.close();
  f.requests[1].resolve({ reportId, jobId });
  await f.ui.flush();
  assert.deepEqual(f.calls.at(-1), {
    command: "launch_failure_ai_cancel",
    args: { reportId, jobId },
  });
  assert.equal(
    f.calls.filter((call) => call.command === "launch_failure_ai_status")
      .length,
    0,
  );
  f.requests.at(-1).resolve({ cancelled: true });
});

test("navigation cancels the old running job and ignores a late response", async () => {
  const f = fixture();
  await running(f);
  const read = f.requests[2];
  f.ui.replace({ scopeKey: "root-b:epoch-two" });
  await f.ui.flush();
  assert.deepEqual(f.calls.at(-1), {
    command: "launch_failure_ai_cancel",
    args: { reportId, jobId },
  });
  read.resolve(status({ output: "OLD ROOT DATA" }));
  await f.ui.flush();
  assert.ok(!content(f.ui.tree).includes("OLD ROOT DATA"));
  f.requests.at(-1).resolve({ cancelled: true });
  f.ui.close();
});

test("stop settles the same job and retains its partial output", async () => {
  const f = fixture();
  await running(f);
  f.requests[2].resolve(status({ output: "Partial evidence" }));
  await f.ui.flush();
  const stop = button(f.ui, "failure.aiStop").props.onClick;
  stop();
  stop();
  await f.ui.flush();
  assert.equal(
    f.calls.filter((call) => call.command === "launch_failure_ai_cancel")
      .length,
    1,
  );
  f.requests.at(-1).resolve({ cancelled: true });
  await f.ui.flush();
  assert.ok(content(f.ui.tree).includes("Partial evidence"));
  assert.ok(content(f.ui.tree).includes(f.ui.i18n.t("failure.aiCancelled")));
  const count = f.calls.length;
  await f.ui.poll();
  assert.equal(f.calls.length, count);
  f.ui.close();
});

test("status from a different report is rejected and the admitted job is cancelled", async () => {
  const f = fixture();
  await running(f);
  f.requests[2].resolve(
    status({ reportId: "b".repeat(64), output: "wrong evidence" }),
  );
  await f.ui.flush();
  assert.ok(!content(f.ui.tree).includes("wrong evidence"));
  assert.ok(content(f.ui.tree).includes(f.ui.i18n.t("failure.wrongRun")));
  assert.equal(f.calls.at(-1).command, "launch_failure_ai_cancel");
  f.requests.at(-1).resolve({ cancelled: true });
  f.ui.close();
});

test("uncertain start failures require a fresh explicit preview rather than retrying", async () => {
  const f = fixture();
  await show(f);
  const click = button(f.ui, "failure.aiSend").props.onClick;
  click();
  await f.ui.flush();
  f.requests[1].reject(new Error("Engine disconnected"));
  await f.ui.flush();
  click();
  await f.ui.poll();
  assert.equal(f.calls.length, 2);
  assert.ok(content(f.ui.tree).includes("Engine disconnected"));
  f.ui.close();
});

test("export completion from a retired view cannot annotate the next report", async () => {
  const f = fixture();
  button(f.ui, "failure.exportReport").props.onClick();
  await f.ui.flush();
  f.ui.replace({ revision: "b".repeat(64) });
  await f.ui.flush();
  f.requests[0].resolve({
    status: "complete",
    path: "/synthetic/old-report.txt",
  });
  await f.ui.flush();
  assert.ok(!content(f.ui.tree).includes(f.ui.i18n.t("failure.exported")));
  f.ui.close();
});

test("real rootApi forwards root identity and Tauri camelCase arguments through the full diagnostic flow", async () => {
  const f = fixture({}, "root-a");
  button(f.ui, "failure.exportReport").props.onClick();
  await f.ui.flush();
  assert.deepEqual(f.calls.at(-1), {
    command: "launch_failure_export",
    args: { expected, revision: reportId, rootId: "root-a" },
  });
  f.requests.at(-1).resolve({ status: "cancelled" });
  await f.ui.flush();
  await show(f);
  assert.deepEqual(f.calls.at(-1), {
    command: "launch_failure_ai_preview",
    args: { expected, revision: reportId, rootId: "root-a" },
  });
  button(f.ui, "failure.aiSend").props.onClick();
  await f.ui.flush();
  assert.deepEqual(f.calls.at(-1), {
    command: "launch_failure_ai_start",
    args: {
      expected,
      revision: reportId,
      rootId: "root-a",
      confirmationToken: "single-use-token",
    },
  });
  f.requests.at(-1).resolve({ jobId, reportId });
  await f.ui.flush();
  assert.deepEqual(f.calls.at(-1), {
    command: "launch_failure_ai_status",
    args: { reportId, jobId },
  });
  f.requests.at(-1).resolve(status());
  await f.ui.flush();
  button(f.ui, "failure.aiStop").props.onClick();
  await f.ui.flush();
  assert.deepEqual(f.calls.at(-1), {
    command: "launch_failure_ai_cancel",
    args: { reportId, jobId },
  });
  f.requests.at(-1).resolve({ cancelled: true });
  await f.ui.flush();
  f.ui.close();
});
